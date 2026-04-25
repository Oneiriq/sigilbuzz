//! Universal Shaping Engine (USE) per-codepoint categorisation.
//!
//! The USE is Microsoft's generalised complex-script shaper. It covers
//! Khmer, Myanmar, Tai Tham, Buginese, Cham, New Tai Lue and several
//! others that do not fit the Arabic / Indic2 moulds. Each codepoint
//! it sees is classified along two axes:
//!
//! - [`UseCategory`] — the role the codepoint plays inside a syllable
//!   (base, halant, vowel, final mark, ...). Drives the syllable state
//!   machine and the feature masking.
//! - [`UsePosition`] — where a mark visually sits relative to its base
//!   (pre-base, above-base, below-base, post-base). Drives the reorder
//!   pass and picks the correct positional feature bucket (`abvf`,
//!   `blwf`, `pstf`, `pref`).
//!
//! sigilbuzz 0.2.0 wired up Khmer, Myanmar, Thai, Lao, and the Jamo
//! subset of Hangul against these tables. 0.7.x extends the coverage
//! to N'Ko, Buginese, Tai Tham, Balinese, Sundanese, Lepcha, Limbu,
//! and Cham. The tables live here rather than inside a script-specific
//! module because the same state machine consumes them for every USE
//! script.
//!
//! # Sources
//!
//! The classification mirrors the columns in the MS-published
//! `IndicSyllabicCategory.txt` / `IndicPositionalCategory.txt` files
//! combined with the USE-specific overrides listed in the MS USE docs.
//! Each script slice was cross-checked against `rustybuzz`'s
//! `ot_shaper_use_table.rs` (reference implementation) so the state
//! machine consumes identical categories.

/// USE per-codepoint category. The mnemonic names mirror the labels
/// from the MS USE documentation so OpenType spec readers can map
/// straight across.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(missing_docs)]
pub enum UseCategory {
    /// Base consonant / independent letter — anchors a syllable.
    B,
    /// Independent vowel.
    IV,
    /// Number — digits and number signs.
    N,
    /// Generic base — default for letters that do not participate in
    /// the syllable cluster (punctuation, signs that stand alone).
    GB,
    /// Repha — reordering ra-equivalent. Myanmar uses this for the
    /// `kinzi` cluster (ra + asat + virama preceding the base).
    R,
    /// Symbol.
    S,
    /// Halant / virama — deletes the inherent vowel of the preceding
    /// base and glues it to the following consonant as a subscript.
    /// Khmer uses U+17D2 COENG, Myanmar U+1039, Hangul none.
    H,
    /// Pre-base vowel sign — renders before the base visually.
    VPre,
    /// Above-base vowel sign.
    VAbv,
    /// Below-base vowel sign.
    VBlw,
    /// Post-base vowel sign — renders after the base visually.
    VPst,
    /// Modifying mark — tone marks, registers, robat, bindu-likes.
    M,
    /// Final mark — syllable-final modifiers (visarga, anusvara).
    FM,
    /// Consonant modifier / medial — Myanmar medial ya/ra/wa/ha,
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
    /// Anything else — treated as a cluster-break / pass-through.
    O,
}

/// Where a mark sits relative to its base. Matches the IPC partition
/// used by the Indic shaper but with the USE-specific pre/below/post
/// split laid out explicitly so the reorder pass can branch cleanly.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(missing_docs)]
pub enum UsePosition {
    /// No positional role — the default for bases, whitespace, marks
    /// that attach at the overall glyph box.
    NotApplicable,
    /// Before the base visually — pre-base matras.
    PreBase,
    /// Above the base — above-base vowel signs and tone marks.
    AboveBase,
    /// Below the base — below-base vowel signs and subscript marks.
    BelowBase,
    /// After the base visually — post-base matras and final marks.
    PostBase,
}

/// Returns the USE positional category for a character.
///
/// Unknown codepoints return [`UsePosition::NotApplicable`].
///
/// The arms are grouped by script rather than by category so the
/// table reads top-to-bottom against the Unicode block layout;
/// clippy's `match_same_arms` lint flags this as mergeable but
/// merging across script boundaries destroys the script-locality
/// that makes the table maintainable.
#[must_use]
#[allow(clippy::match_same_arms)]
#[allow(clippy::too_many_lines)]
pub const fn use_position(ch: char) -> UsePosition {
    let cp = ch as u32;
    match cp {
        // --- Khmer dependent vowel signs --------------------------
        // PostBase: 17B6 sign aa, 17C4/17C5 signs oo/au
        // AboveBase: 17B7..17BA (i/ii/y/yy) and 17BE..17C0 (oe/ya/ie)
        // BelowBase: 17BB..17BD (u/uu/ua)
        // PreBase:   17C1..17C3 (e/ai/am-prefix)
        0x17B6 | 0x17C4 | 0x17C5 => UsePosition::PostBase,
        0x17B7..=0x17BA | 0x17BE..=0x17C0 => UsePosition::AboveBase,
        0x17BB..=0x17BD => UsePosition::BelowBase,
        0x17C1..=0x17C3 => UsePosition::PreBase,

        // --- Myanmar dependent vowel signs ------------------------
        //   VPst: 102B sign tall aa, 102C sign aa,
        //         1056/1057 signs vocalic r/rr, 1062..1064 shan post,
        //         1067/1068/1083/1084, 109C sign shan aa
        //   VAbv: 102D sign i, 102E sign ii, 1032..1035,
        //         1071..1074 shan above-base,
        //         1085..1086, 108D, 109D,
        //         AA7C, A9E5
        //   VBlw: 102F sign u, 1030 sign uu,
        //         1058/1059 vocalic l/ll
        //   VPre: 1031 sign e
        //
        //   CM medials (103B..103E, 105E..1060, 1082) carry no
        //   positional role in the USE tables — they attach as
        //   consonant modifiers and the font chooses their visual
        //   position via GSUB.
        0x102B
        | 0x102C
        | 0x1056
        | 0x1057
        | 0x1062..=0x1064
        | 0x1067..=0x1068
        | 0x1083..=0x1084
        | 0x109C => UsePosition::PostBase,
        0x102D
        | 0x102E
        | 0x1032..=0x1035
        | 0x1071..=0x1074
        | 0x1085..=0x1086
        | 0x108D
        | 0x109D
        | 0xAA7C
        | 0xA9E5 => UsePosition::AboveBase,
        0x102F | 0x1030 | 0x1058 | 0x1059 => UsePosition::BelowBase,
        // Medial ra (U+103C) renders before the base in Myanmar —
        // USE classifies it as pre-base for reorder purposes. Medial
        // ya / wa / ha (U+103B / 103D / 103E) keep NotApplicable;
        // the font's `blwf` / `pstf` features place them.
        0x1031 | 0x103C => UsePosition::PreBase,

        // --- Thai dependent vowel signs ---------------------------
        //   VPre: 0E40..0E44 (sara e, ae, o, ai-maimuan, ai-maimalai)
        //   VAbv: 0E31 mai han-akat, 0E34..0E37, 0E47..0E4E
        //   VBlw: 0E38..0E3A
        //   VPst: 0E30 sara a, 0E32 sara aa, 0E33 sara am (composed)
        0x0E40..=0x0E44 => UsePosition::PreBase,
        0x0E31 | 0x0E34..=0x0E37 | 0x0E47..=0x0E4E => UsePosition::AboveBase,
        0x0E38..=0x0E3A => UsePosition::BelowBase,
        0x0E30 | 0x0E32 | 0x0E33 => UsePosition::PostBase,

        // --- Lao dependent vowel signs ----------------------------
        //   VPre: 0EC0..0EC4
        //   VAbv: 0EB1, 0EB4..0EB7, 0EC8..0ECD (tone marks)
        //   VBlw: 0EB8, 0EB9
        //   VPst: 0EB0, 0EB2, 0EB3 (lao am, composed)
        0x0EC0..=0x0EC4 => UsePosition::PreBase,
        0x0EB1 | 0x0EB4..=0x0EB7 | 0x0EC8..=0x0ECD => UsePosition::AboveBase,
        0x0EB8 | 0x0EB9 => UsePosition::BelowBase,
        0x0EB0 | 0x0EB2 | 0x0EB3 => UsePosition::PostBase,

        // --- N'Ko (U+07C0..U+07FF) -------------------------------
        // N'Ko marks all sit above their base. The dantayalan
        // (07FD) sits below. No pre-base or post-base vowels — N'Ko
        // is alphabetic, not Brahmic.
        0x07EB..=0x07F3 => UsePosition::AboveBase,
        0x07FD => UsePosition::BelowBase,

        // --- Buginese (U+1A00..U+1A1F) ---------------------------
        // 1A17 vowel-i (above), 1A18 vowel-u (below),
        // 1A19 vowel-e (pre-base), 1A1A vowel-o (post-base),
        // 1A1B vowel-ae (above).
        0x1A17 | 0x1A1B => UsePosition::AboveBase,
        0x1A18 => UsePosition::BelowBase,
        0x1A19 => UsePosition::PreBase,
        0x1A1A => UsePosition::PostBase,

        // --- Tai Tham (U+1A20..U+1AAF) ---------------------------
        // 1A55 medial ra (pre-base), 1A56 medial la (below),
        // 1A57 a-mode (post), 1A58..1A5E above-base signs,
        // 1A60 sakot (halant — NotApplicable for position),
        // 1A61 a, 1A62 mai sat (above), 1A63..1A64 aa (post),
        // 1A65..1A68 above (i/ii/ue/uee), 1A69..1A6A below (u/uu),
        // 1A6B above (o), 1A6C..1A72 various, 1A6E..1A72 pre-base
        // vowels, 1A73..1A74 above, 1A75..1A7C tone above,
        // 1A7F dot below.
        0x1A55 | 0x1A6E..=0x1A72 => UsePosition::PreBase,
        0x1A58..=0x1A5E | 0x1A62 | 0x1A65..=0x1A68 | 0x1A6B | 0x1A73..=0x1A74 | 0x1A75..=0x1A7C => {
            UsePosition::AboveBase
        }
        0x1A56 | 0x1A69..=0x1A6A | 0x1A7F => UsePosition::BelowBase,
        0x1A57 | 0x1A63..=0x1A64 | 0x1A6C..=0x1A6D => UsePosition::PostBase,

        // --- Balinese (U+1B00..U+1B7F) ---------------------------
        // 1B00..1B03 anusvara/visarga (above except 1B03 which is
        // post in some sources; treat as above for placement).
        // 1B34 rerekan (above), 1B35 sign tedung (post),
        // 1B36..1B38 above (i/ii/u variants), 1B39..1B3A below,
        // 1B3B above, 1B3C below,
        // 1B3D sign la e (above), 1B3E sign le (pre-base),
        // 1B3F sign le tedung (pre-base — sign le + tedung
        //   composed; renders before the base then a tedung after),
        // 1B40 sign taa-le (pre-base — like Devanagari sign O),
        // 1B41 sign taa-le tedung (pre-base),
        // 1B42 above (sign ie), 1B43 above (sign ai),
        // 1B44 adeg adeg (halant — NotApplicable),
        // 1B6B..1B73 musical/above, 1B80..1B82 stay in 1B80 block.
        0x1B36..=0x1B38 | 0x1B3B | 0x1B3D | 0x1B42 | 0x1B43 | 0x1B6B..=0x1B73 => {
            UsePosition::AboveBase
        }
        0x1B3E..=0x1B41 => UsePosition::PreBase,
        0x1B34 => UsePosition::AboveBase,
        0x1B39..=0x1B3A | 0x1B3C => UsePosition::BelowBase,
        0x1B35 => UsePosition::PostBase,

        // --- Sundanese (U+1B80..U+1BBF) --------------------------
        // 1B80 panyecek (above anusvara), 1B81 panglayar (above),
        // 1B82 pangwisad (post),
        // 1BA1 pamingkal (post), 1BA2 panyakra (below),
        // 1BA3 panyikuh (below),
        // 1BA4 vowel sign i (above), 1BA5 vowel sign u (below),
        // 1BA6 vowel sign e (pre-base), 1BA7 vowel sign aa (post),
        // 1BA8..1BA9 above (eu/ae), 1BAA pamaaeh (post),
        // 1BAB virama (halant), 1BAC..1BAD consonant signs (above).
        0x1B80..=0x1B81 | 0x1BA4 | 0x1BA8..=0x1BA9 | 0x1BAC..=0x1BAD => UsePosition::AboveBase,
        0x1BA2..=0x1BA3 | 0x1BA5 => UsePosition::BelowBase,
        0x1BA6 => UsePosition::PreBase,
        0x1B82 | 0x1BA1 | 0x1BA7 | 0x1BAA => UsePosition::PostBase,

        // --- Lepcha (U+1C00..U+1C4F) -----------------------------
        // 1C24..1C25 subjoined consonants (post),
        // 1C26 sign i (post), 1C27 sign o (pre-base),
        // 1C28 sign on (post), 1C29 sign u (below),
        // 1C2A sign uu (post), 1C2B sign uu (post),
        // 1C2C sign u-below (below), 1C2D sign e (post),
        // 1C2E sign ee (post), 1C2F sign ai (post),
        // 1C30 sign yy (post),
        // 1C34..1C35 consonant signs (post),
        // 1C36 ran (above), 1C37 nukta (below),
        // 1C40..1C49 digits.
        0x1C27 => UsePosition::PreBase,
        0x1C36 => UsePosition::AboveBase,
        0x1C29 | 0x1C2C | 0x1C37 => UsePosition::BelowBase,
        0x1C24..=0x1C26 | 0x1C28 | 0x1C2A..=0x1C2B | 0x1C2D..=0x1C35 => UsePosition::PostBase,

        // --- Limbu (U+1900..U+194F) ------------------------------
        // 1920..1922 above (a/i/u), 1923..1924 below (ee/ai),
        // 1925..1926 above (oo/au), 1927..1928 below (e/o),
        // 1929..192B subjoined consonants (below),
        // 1930..1938 small / final letters (post),
        // 1939..193B signs (above/below). 193B sa-i (below),
        // 1939..193A above tone marks.
        0x1920..=0x1922 | 0x1925..=0x1926 | 0x1939..=0x193A => UsePosition::AboveBase,
        0x1923..=0x1924 | 0x1927..=0x192B | 0x193B => UsePosition::BelowBase,
        0x1930..=0x1938 => UsePosition::PostBase,

        // --- Cham (U+AA00..U+AA5F) -------------------------------
        // AA29..AA2E above (aa/i/ii/ei/u),
        // AA2F..AA30 pre-base (oe/o — render visually before the
        //   base consonant; the IndicPositionalCategory column
        //   marks them Top_And_Left in the Unicode Standard, but
        //   the USE places them in the pre-base bucket — same as
        //   rustybuzz),
        // AA31..AA32 above (ai/au),
        // AA33 post (medial ya), AA34 below (medial ra),
        // AA35..AA36 below (medial la/wa),
        // AA43 final ng (post), AA4C consonant sign (above),
        // AA4D consonant sign (post).
        0xAA29..=0xAA2E | 0xAA31..=0xAA32 | 0xAA4C => UsePosition::AboveBase,
        0xAA34..=0xAA36 => UsePosition::BelowBase,
        0xAA2F..=0xAA30 => UsePosition::PreBase,
        0xAA33 | 0xAA43 | 0xAA4D => UsePosition::PostBase,

        // --- Brahmi (U+11000..U+1107F) ---------------------------
        // 11000 sign candrabindu (above), 11001 sign anusvara (above),
        // 11002 sign visarga (post), 11038..11045 vowel signs:
        //   11038 sign aa (post), 11039 sign bb-i (above),
        //   1103A sign ii (above), 1103B sign u (below),
        //   1103C sign uu (below), 1103D sign vocalic r (below),
        //   1103E sign vocalic rr (below), 1103F sign vocalic l (below),
        //   11040 sign vocalic ll (below), 11041 sign e (above),
        //   11042 sign ai (above), 11043 sign o (post),
        //   11044 sign au (post), 11045 sign virama (NotApplicable).
        // 11073 vowel sign old tamil short e (above),
        // 11074 vowel sign old tamil short o (post).
        0x11000..=0x11001 | 0x11039..=0x1103A | 0x11041..=0x11042 | 0x11073 => UsePosition::AboveBase,
        0x1103B..=0x11040 => UsePosition::BelowBase,
        0x11002 | 0x11038 | 0x11043..=0x11044 | 0x11074 => UsePosition::PostBase,

        // --- Sharada (U+11180..U+111DF) --------------------------
        // 11180 candrabindu (above), 11181 anusvara (above),
        // 11182 visarga (post), 111B3..111BF vowel signs:
        //   111B3 sign aa (post), 111B4 sign i (above),
        //   111B5 sign ii (above), 111B6..111B9 below,
        //   111BA sign vocalic ll (below), 111BB sign e (post),
        //   111BC sign ai (post), 111BD sign o (post),
        //   111BE sign au (post), 111BF sign vowel sign aw (post),
        //   111C0 sign virama (NotApplicable).
        // 111CA nukta (below), 111CB vowel modifier mark (above),
        // 111CC extra short vowel mark (above),
        // 111CD sutra mark (above), 111CE sign vowel modifier (above),
        // 111CF sign inverted candrabindu (above).
        0x11180..=0x11181 | 0x111CB..=0x111CF => UsePosition::AboveBase,
        0x111B6..=0x111BA | 0x111CA => UsePosition::BelowBase,
        0x111B4..=0x111B5 => UsePosition::PreBase,
        0x11182 | 0x111B3 | 0x111BB..=0x111BF => UsePosition::PostBase,

        // --- Khojki (U+11200..U+1124F) ---------------------------
        // 1122C..1122E vowel signs (post — sign aa/i/ii),
        // 1122F sign u (below), 11230 sign e (above),
        // 11231 sign ai (above), 11232..11233 sign o/au (post),
        // 11234 anusvara (above), 11235 virama (NotApplicable),
        // 11236 nukta (below), 11237 shadda (above).
        0x11230..=0x11231 | 0x11234 | 0x11237 => UsePosition::AboveBase,
        0x1122F | 0x11236 => UsePosition::BelowBase,
        0x1122C..=0x1122E | 0x11232..=0x11233 => UsePosition::PostBase,

        // --- Tirhuta (U+11480..U+114DF) --------------------------
        // 114B0 sign aa (post), 114B1 sign i (post),
        // 114B2 sign ii (post), 114B3..114B8 below (u/uu/vocalic r/rr/l/ll),
        // 114B9 sign e (pre-base — Tirhuta places sign-e visually
        //   before the base, like Bengali sign-e),
        // 114BA sign short e (above),
        // 114BB sign ai (post), 114BC sign o (pre — like sign-e),
        // 114BD sign short o (post), 114BE sign au (post),
        // 114BF sign candrabindu (above), 114C0 sign anusvara (above),
        // 114C1 sign visarga (post), 114C2 sign virama (NotApplicable),
        // 114C3 sign nukta (below).
        0x114BA | 0x114BF..=0x114C0 => UsePosition::AboveBase,
        0x114B3..=0x114B8 | 0x114C3 => UsePosition::BelowBase,
        0x114B9 | 0x114BC => UsePosition::PreBase,
        0x114B0..=0x114B2 | 0x114BB | 0x114BD..=0x114BE | 0x114C1 => UsePosition::PostBase,

        // --- Modi (U+11600..U+1165F) -----------------------------
        // 11630..11632 vowel signs (post — sign aa/i/ii),
        // 11633..11637 below (u/uu/vocalic r/rr/l),
        // 11638 sign vocalic ll (below),
        // 11639..1163A above (e/ai),
        // 1163B..1163C post (o/au),
        // 1163D anusvara (above), 1163E visarga (post),
        // 1163F virama (NotApplicable),
        // 11640 ardhacandra (above).
        0x11639..=0x1163A | 0x1163D | 0x11640 => UsePosition::AboveBase,
        0x11633..=0x11638 => UsePosition::BelowBase,
        0x11630..=0x11632 | 0x1163B..=0x1163C | 0x1163E => UsePosition::PostBase,

        // Currency + signs — no positional role (the currency is a
        // base glyph itself).
        _ => UsePosition::NotApplicable,
    }
}

/// Returns the USE category for a character.
///
/// The table is structured as a series of range matches so additional
/// USE scripts (Tai Tham, Buginese, Cham, ...) can be added as
/// additional match arms without touching the state machine.
///
/// Unknown codepoints return [`UseCategory::O`]. sigilbuzz only has
/// data for scripts it can actively shape; everything else flows
/// through the generic GSUB/GPOS pipeline.
///
/// # Coverage
///
/// Khmer (U+1780..U+17FF, U+19E0..U+19FF), Myanmar (U+1000..U+109F plus
/// Extended-A U+AA60..U+AA7F and Extended-B U+A9E0..U+A9FF), Thai
/// (U+0E00..U+0E7F), Lao (U+0E80..U+0EFF), and the Hangul Jamo blocks
/// (U+1100..U+11FF, U+A960..U+A97F, U+D7B0..U+D7FF) are covered. Format
/// characters and variation selectors carry their shared USE categories.
///
/// Arms are grouped by script / Unicode block — `match_same_arms` is
/// silenced so the table reads top-to-bottom against the block layout
/// and a reviewer can check each script slice in isolation.
#[must_use]
#[allow(clippy::match_same_arms)]
#[allow(clippy::too_many_lines)]
pub const fn use_category(ch: char) -> UseCategory {
    let cp = ch as u32;
    match cp {
        // --- Shared format characters -----------------------------
        0x200C => UseCategory::ZWNJ,
        0x200D => UseCategory::ZWJ,
        0xFE00..=0xFE0F | 0xE0100..=0xE01EF => UseCategory::VS,

        // --- Khmer -----------------------------------------------
        // Khmer consonants (U+1780..U+17A4, incl. deprecated inherent-
        // vowel consonants 17A3 / 17A4 which rustybuzz still treats
        // as bases) plus U+17DC avakrahasanya.
        0x1780..=0x17A4 | 0x17DC => UseCategory::B,
        // Independent vowels U+17A5..U+17B3.
        0x17A5..=0x17B3 => UseCategory::IV,
        // Dotted circle, Khmer inherent-vowel signs (17B4/17B5), the
        // riel currency (17DB), and the Khmer Symbols block.
        0x25CC | 0x17B4..=0x17B5 | 0x17DB | 0x19E0..=0x19FF => UseCategory::GB,
        0x17B6 | 0x17C4 | 0x17C5 => UseCategory::VPst,
        0x17B7..=0x17BA | 0x17BE..=0x17C0 => UseCategory::VAbv,
        0x17BB..=0x17BD => UseCategory::VBlw,
        0x17C1..=0x17C3 => UseCategory::VPre,
        0x17C6..=0x17C8 => UseCategory::FM,
        0x17C9..=0x17D1 | 0x17D3 | 0x17D7 | 0x17DD => UseCategory::M,
        0x17D2 => UseCategory::H,
        0x17E0..=0x17E9 | 0x17F0..=0x17F9 => UseCategory::N,

        // --- Myanmar main block (U+1000..U+109F) ------------------
        // Consonants 1000..1020, plus the late-added ones 1050..1055,
        // 105A..105D, 1061, 1065..1066, 106E..1070, 1075..1081, 108E.
        0x1000..=0x1020
        | 0x103F
        | 0x1050..=0x1055
        | 0x105A..=0x105D
        | 0x1061
        | 0x1065..=0x1066
        | 0x106E..=0x1070
        | 0x1075..=0x1081
        | 0x108E => UseCategory::B,
        // Independent vowels 1021..102A (incl. i, ii, u, uu, e, o, ai).
        0x1021..=0x102A => UseCategory::IV,
        // Vowel signs split by visual position.
        0x102B | 0x102C => UseCategory::VPst,
        0x102D | 0x102E => UseCategory::VAbv,
        0x102F | 0x1030 => UseCategory::VBlw,
        0x1031 => UseCategory::VPre,
        0x1032..=0x1035 => UseCategory::VAbv,
        // Anusvara, dot below, visarga — syllable-final marks.
        0x1036..=0x1038 => UseCategory::FM,
        // Virama (U+1039) + asat (U+103A). asat is the "explicit
        // virama" that doesn't trigger subjoining; classifying it as
        // H lets the state machine end the syllable cleanly.
        0x1039..=0x103A => UseCategory::H,
        // Medial consonants — ya (103B), ra (103C), wa (103D), ha
        // (103E). These are not full halant-joined consonants, they
        // are special Myanmar modifiers that attach to the preceding
        // base via `pref`/`blwf`/`pstf`.
        0x103B..=0x103E => UseCategory::CM,
        // Myanmar digits + shan digits (1040..1049, 1090..1099).
        0x1040..=0x1049 | 0x1090..=0x1099 => UseCategory::N,
        // Myanmar punctuation + symbols (104A..104F).
        0x104A..=0x104F => UseCategory::GB,
        // Vocalic-r/l vowel signs.
        0x1056..=0x1057 => UseCategory::VPst,
        0x1058..=0x1059 => UseCategory::VBlw,
        // Mon medial consonants (105E..1060).
        0x105E..=0x1060 => UseCategory::CM,
        // Shan vowel signs (1062..1064 post, 1067..1068 post,
        // 1071..1074 above, 1083..1084 post, 1085..1086 above,
        // 108D above).
        0x1062..=0x1064 | 0x1067..=0x1068 | 0x1083..=0x1084 => UseCategory::VPst,
        0x1071..=0x1074 | 0x1085..=0x1086 | 0x108D => UseCategory::VAbv,
        // Shan tone marks (1069..106D, 1087..108C, 108F, 109A..109B).
        0x1069..=0x106D | 0x1087..=0x108C | 0x108F | 0x109A..=0x109B => UseCategory::M,
        // Myanmar medial mon la (1082) — consonant modifier.
        0x1082 => UseCategory::CM,
        // Aiton / Khamti sign (109C post, 109D above).
        0x109C => UseCategory::VPst,
        0x109D => UseCategory::VAbv,
        // Symbols at block end (109E..109F).
        0x109E..=0x109F => UseCategory::GB,

        // --- Myanmar Extended-A (U+AA60..U+AA7F) -----------------
        // Shan/Mon/Khamti consonants 0xAA60..0xAA6F,
        // myanmar letter khamti reduplication signs 0xAA70 (sign),
        // 0xAA71..0xAA76 more consonants,
        // 0xAA77..0xAA79 symbols,
        // 0xAA7A letter aiton ra, 0xAA7B sign aiton pa (M),
        // 0xAA7C sign aiton ai (VAbv), 0xAA7D sign aiton bhaa (M),
        // 0xAA7E..0xAA7F shan consonants.
        0xAA60..=0xAA6F | 0xAA71..=0xAA76 | 0xAA7A | 0xAA7E..=0xAA7F => UseCategory::B,
        0xAA70 => UseCategory::CM,
        0xAA77..=0xAA79 => UseCategory::GB,
        0xAA7B | 0xAA7D => UseCategory::M,
        0xAA7C => UseCategory::VAbv,

        // --- Myanmar Extended-B (U+A9E0..U+A9FF) -----------------
        // Shan consonants 0xA9E0..0xA9E4, sign shan saw 0xA9E5
        // (VAbv), letter sign (0xA9E6 — modifier), more consonants
        // 0xA9E7..0xA9EF, Shan digits 0xA9F0..0xA9F9, consonants
        // 0xA9FA..0xA9FE, reserved 0xA9FF.
        0xA9E0..=0xA9E4 | 0xA9E7..=0xA9EF | 0xA9FA..=0xA9FE => UseCategory::B,
        0xA9E5 => UseCategory::VAbv,
        0xA9E6 => UseCategory::CM,
        0xA9F0..=0xA9F9 => UseCategory::N,

        // --- Thai (U+0E00..U+0E7F) -------------------------------
        // Consonants 0E01..0E2E (incl. ng, cho, phoom, ro, lo, wo,
        // so, ho, o, ng-obsolete). No explicit halant — Thai has no
        // subjoining.
        0x0E01..=0x0E2E => UseCategory::B,
        // Independent vowels 0E2F paiyannoi, 0E46 maiyamok (B-like
        // repeat mark). Treat 0E2F as GB (punctuation-style) and
        // 0E46 as GB — both can stand alone.
        0x0E2F | 0x0E46 | 0x0E4F | 0x0E5A..=0x0E5B => UseCategory::GB,
        // Thai tonal / vowel placement.
        0x0E30 | 0x0E32 | 0x0E33 => UseCategory::VPst,
        0x0E31 | 0x0E34..=0x0E37 => UseCategory::VAbv,
        0x0E38..=0x0E39 => UseCategory::VBlw,
        // Pinthu (0E3A) — silencer, above-base in Thai.
        0x0E3A => UseCategory::VBlw,
        // Pre-base vowels.
        0x0E40..=0x0E44 => UseCategory::VPre,
        // Thai currency signs (0E3F baht). GB.
        0x0E3F => UseCategory::GB,
        // Mai Taikhu / Mai Ek / Mai Tho / Mai Tri / Mai Chattawa /
        // Thanthakhat / Nikhahit / Yamakkan — tone / final marks.
        // 0E45 (lakkhangyao) — long-vowel extender (VPst-ish).
        0x0E45 => UseCategory::VPst,
        0x0E47..=0x0E4E => UseCategory::M,
        // Thai digits.
        0x0E50..=0x0E59 => UseCategory::N,

        // --- Lao (U+0E80..U+0EFF) -------------------------------
        // Lao consonants (with gaps — 0E81, 0E82, 0E84, 0E86..0E8A,
        // 0E8C..0EA3, 0EA5, 0EA7..0EAE, 0EB0 boundary, etc.). For
        // simplicity treat every codepoint in the consonant sub-
        // range as B; unassigned codepoints will fall through to
        // the wildcard below.
        0x0E81..=0x0E82
        | 0x0E84
        | 0x0E86..=0x0E8A
        | 0x0E8C..=0x0EA3
        | 0x0EA5
        | 0x0EA7..=0x0EAE
        | 0x0EDC..=0x0EDF => UseCategory::B,
        // Lao vowel signs.
        0x0EAF => UseCategory::GB,
        0x0EB0 | 0x0EB2 | 0x0EB3 => UseCategory::VPst,
        0x0EB1 | 0x0EB4..=0x0EB7 => UseCategory::VAbv,
        0x0EB8..=0x0EB9 => UseCategory::VBlw,
        // 0EBA (sign pali virama) — used as silencer, like pinthu.
        0x0EBA => UseCategory::VBlw,
        // Lao semivowels (0EBB..0EBC) — above-base consonant-like
        // modifier (wo/yo attached).
        0x0EBB..=0x0EBC => UseCategory::CM,
        0x0EBD => UseCategory::B,
        0x0EC0..=0x0EC4 => UseCategory::VPre,
        0x0EC6 => UseCategory::GB,
        0x0EC8..=0x0ECD => UseCategory::M,
        0x0ED0..=0x0ED9 => UseCategory::N,

        // --- N'Ko (U+07C0..U+07FF) -------------------------------
        // N'Ko digits (07C0..07C9), letters (07CA..07EA),
        // tone / combining marks (07EB..07F3, 07FD), low-tone
        // letters (07F4..07F5), exclam/question marks (07F8..07F9),
        // lajanyalan (07FA — consonant modifier / TATWEEL-like).
        0x07C0..=0x07C9 => UseCategory::N,
        0x07CA..=0x07EA | 0x07F4..=0x07F5 => UseCategory::B,
        0x07EB..=0x07F3 | 0x07FD => UseCategory::M,
        0x07F6..=0x07F9 => UseCategory::GB,
        0x07FA => UseCategory::CM,

        // --- Buginese (U+1A00..U+1A1F) ---------------------------
        0x1A00..=0x1A16 => UseCategory::B,
        0x1A17 | 0x1A1B => UseCategory::VAbv,
        0x1A18 => UseCategory::VBlw,
        0x1A19 => UseCategory::VPre,
        0x1A1A => UseCategory::VPst,
        0x1A1E..=0x1A1F => UseCategory::GB,

        // --- Tai Tham (U+1A20..U+1AAF) ---------------------------
        // Consonants 1A20..1A4C, independent vowels 1A4D..1A52,
        // sign la-tang lai 1A53, sign sakot 1A60 (halant),
        // medial ra 1A55 (CM, pre-base), medial la 1A56 (CM, below),
        // medial wa 1A54 (CM), sign mai sat 1A57 (post),
        // signs 1A58..1A5E (above modifiers — CM),
        // sa 1A5F (final consonant — CM),
        // vowel signs 1A61..1A6C (mix), pre-base 1A6E..1A72,
        // 1A73..1A74 above-base extensions, 1A75..1A7C tone marks,
        // 1A7F dot below, digits 1A80..1A89, 1A90..1A99.
        0x1A20..=0x1A4C => UseCategory::B,
        0x1A4D..=0x1A52 => UseCategory::IV,
        0x1A53..=0x1A54 | 0x1A55..=0x1A56 | 0x1A58..=0x1A5E => UseCategory::CM,
        0x1A57 | 0x1A63..=0x1A64 | 0x1A6C..=0x1A6D => UseCategory::VPst,
        0x1A5F => UseCategory::CM,
        0x1A60 => UseCategory::H,
        0x1A61 | 0x1A62 | 0x1A65..=0x1A68 | 0x1A6B | 0x1A73..=0x1A74 => UseCategory::VAbv,
        0x1A69..=0x1A6A => UseCategory::VBlw,
        0x1A6E..=0x1A72 => UseCategory::VPre,
        0x1A75..=0x1A7C => UseCategory::M,
        0x1A7F => UseCategory::M,
        0x1A80..=0x1A89 | 0x1A90..=0x1A99 => UseCategory::N,
        0x1AA0..=0x1AA6 | 0x1AA8..=0x1AAD => UseCategory::GB,
        0x1AA7 => UseCategory::M,

        // --- Balinese (U+1B00..U+1B7F) ---------------------------
        // 1B00..1B03 signs (above), 1B04 visarga (post-base FM),
        // 1B05..1B33 letters,
        // 1B34 rerekan (above modifier — M),
        // 1B35..1B43 vowel signs (mix),
        // 1B44 adeg adeg (halant),
        // 1B45..1B4F more letters,
        // 1B50..1B59 digits,
        // 1B5A..1B6A punctuation/symbols,
        // 1B6B..1B73 musical signs (above marks),
        // 1B74..1B7C symbols.
        0x1B00..=0x1B03 => UseCategory::M,
        0x1B04 => UseCategory::FM,
        0x1B05..=0x1B33 | 0x1B45..=0x1B4F => UseCategory::B,
        0x1B34 => UseCategory::M,
        0x1B35 => UseCategory::VPst,
        0x1B36..=0x1B38 | 0x1B3B | 0x1B3D | 0x1B42 | 0x1B43 => UseCategory::VAbv,
        0x1B3E..=0x1B41 => UseCategory::VPre,
        0x1B39..=0x1B3A | 0x1B3C => UseCategory::VBlw,
        0x1B44 => UseCategory::H,
        0x1B50..=0x1B59 => UseCategory::N,
        0x1B5A..=0x1B6A | 0x1B74..=0x1B7C => UseCategory::GB,
        0x1B6B..=0x1B73 => UseCategory::M,

        // --- Sundanese (U+1B80..U+1BBF) --------------------------
        // 1B80 panyecek (anusvara — M),
        // 1B81 panglayar (M),
        // 1B82 pangwisad (FM),
        // 1B83..1B89 independent vowels,
        // 1B8A..1BA0 letters,
        // 1BA1 pamingkal (post-base medial — CM),
        // 1BA2..1BA3 panyakra/panyikuh (below medials — CM),
        // 1BA4 vowel-i (above), 1BA5 vowel-u (below),
        // 1BA6 vowel-e (pre), 1BA7 vowel-aa (post),
        // 1BA8..1BA9 vowel-eu/ae (above),
        // 1BAA pamaaeh (post — final mark / virama-equivalent).
        //   In Unicode 6.1+, 1BAA is given Indic_Syllabic_Category
        //   = Pure_Killer (ie a virama), not a final mark; rustybuzz
        //   classifies it as H. Track that here.
        // 1BAB..1BAD additional consonant signs (above).
        // 1BAE..1BAF more letters.
        // 1BB0..1BB9 digits. 1BBA..1BBF Sundanese symbols.
        0x1B80..=0x1B81 => UseCategory::M,
        0x1B82 => UseCategory::FM,
        0x1B83..=0x1B89 => UseCategory::IV,
        0x1B8A..=0x1BA0 | 0x1BAE..=0x1BAF => UseCategory::B,
        0x1BA1..=0x1BA3 => UseCategory::CM,
        0x1BA4 => UseCategory::VAbv,
        0x1BA5 => UseCategory::VBlw,
        0x1BA6 => UseCategory::VPre,
        0x1BA7 => UseCategory::VPst,
        0x1BA8..=0x1BA9 => UseCategory::VAbv,
        0x1BAA => UseCategory::H,
        0x1BAB => UseCategory::H,
        0x1BAC..=0x1BAD => UseCategory::M,
        0x1BB0..=0x1BB9 => UseCategory::N,
        0x1BBA..=0x1BBF | 0x1CC0..=0x1CCF => UseCategory::GB,

        // --- Lepcha (U+1C00..U+1C4F) -----------------------------
        // 1C00..1C23 letters,
        // 1C24..1C2B subjoined consonants (CM — they sit below or
        // post a base, attached via halant-like behaviour),
        // 1C2C..1C2F vowel signs,
        // 1C30..1C33 vowel signs (post),
        // 1C34..1C35 consonant signs (post),
        // 1C36 ran (above tone), 1C37 nukta (below — M),
        // 1C3B..1C3F punctuation,
        // 1C40..1C49 digits,
        // 1C4D..1C4F more letters.
        0x1C00..=0x1C23 | 0x1C4D..=0x1C4F => UseCategory::B,
        0x1C24..=0x1C25 => UseCategory::CM,
        0x1C26 => UseCategory::VPst,
        0x1C27 => UseCategory::VPre,
        0x1C28 => UseCategory::VPst,
        0x1C29 => UseCategory::VBlw,
        0x1C2A..=0x1C2B => UseCategory::VPst,
        0x1C2C => UseCategory::VBlw,
        0x1C2D..=0x1C33 => UseCategory::VPst,
        0x1C34..=0x1C35 => UseCategory::CM,
        0x1C36 => UseCategory::M,
        0x1C37 => UseCategory::M,
        0x1C3B..=0x1C3F => UseCategory::GB,
        0x1C40..=0x1C49 => UseCategory::N,

        // --- Limbu (U+1900..U+194F) ------------------------------
        // 1900..191F letters,
        // 1920..1922 above (a/i/u),
        // 1923..1928 below (ee/ai/oo/au/e/o — wait some are above),
        //   actually 1925..1926 above (oo/au), 1923..1924 below
        //   (ee/ai), 1927..1928 below (e/o).
        // 1929..192B subjoined (CM, below — yya/ra/sa subjoined),
        // 1930..1938 small / final letters (CM, post),
        // 1939..193B tone / dot marks (M),
        // 1940 sign loo, 1944..1945 punctuation.
        // 1946..194F digits.
        0x1900..=0x191F => UseCategory::B,
        0x1920..=0x1922 | 0x1925..=0x1926 => UseCategory::VAbv,
        0x1923..=0x1924 | 0x1927..=0x1928 => UseCategory::VBlw,
        0x1929..=0x192B => UseCategory::CM,
        0x1930..=0x1938 => UseCategory::CM,
        0x1939..=0x193B => UseCategory::M,
        0x1940 | 0x1944..=0x1945 => UseCategory::GB,
        0x1946..=0x194F => UseCategory::N,

        // --- Cham (U+AA00..U+AA5F) -------------------------------
        // AA00..AA28 letters (incl. independent vowels AA00..AA05),
        // AA29..AA2E vowel signs (above — aa/i/ii/ei/u),
        // AA2F..AA30 vowel signs (post — oe/o),
        // AA31..AA32 vowel signs (above — ai/au),
        // AA33 medial ya (post — CM),
        // AA34..AA36 medial ra/la/wa (below — CM),
        // AA40..AA42 final consonants (CM, post),
        // AA43 final ng (FM/post),
        // AA44..AA4B more final consonants (CM),
        // AA4C consonant sign (above), AA4D consonant sign (post),
        // AA50..AA59 digits, AA5C..AA5F punctuation.
        0xAA00..=0xAA05 => UseCategory::IV,
        0xAA06..=0xAA28 => UseCategory::B,
        0xAA29..=0xAA2E | 0xAA31..=0xAA32 => UseCategory::VAbv,
        0xAA2F..=0xAA30 => UseCategory::VPre,
        0xAA33..=0xAA36 => UseCategory::CM,
        0xAA40..=0xAA42 | 0xAA44..=0xAA4B => UseCategory::CM,
        0xAA43 => UseCategory::FM,
        0xAA4C => UseCategory::M,
        0xAA4D => UseCategory::VPst,
        0xAA50..=0xAA59 => UseCategory::N,
        0xAA5C..=0xAA5F => UseCategory::GB,

        // --- Brahmi (U+11000..U+1107F) ---------------------------
        // Historical script of the Indian subcontinent (3rd century BCE);
        // ancestor of every Brahmic script. SMP block.
        // 11000 candrabindu (M — above-base modifier),
        // 11001 anusvara (M),
        // 11002 visarga (FM — final mark),
        // 11003..11037: 11003..11005 independent vowels,
        //   11006..11037 consonants.
        // 11038..11045 dependent vowel signs (mix of above/below/post),
        // 11046 virama (H),
        // 11047..1104D punctuation (GB),
        // 11052..11065 number signs (N — Brahmi numeric system),
        // 11066..1106F digits (N),
        // 11070 old tamil virama (H — Pulli sign),
        // 11071..11072 old tamil short e/o (IV),
        // 11073..11074 old tamil short e/o vowel signs (above/post),
        // 11075 old tamil lla (B).
        0x11000..=0x11001 => UseCategory::M,
        0x11002 => UseCategory::FM,
        0x11003..=0x11005 => UseCategory::IV,
        0x11006..=0x11037 => UseCategory::B,
        0x11038 | 0x11043..=0x11044 | 0x11074 => UseCategory::VPst,
        0x11039..=0x1103A | 0x11041..=0x11042 | 0x11073 => UseCategory::VAbv,
        0x1103B..=0x11040 => UseCategory::VBlw,
        0x11046 | 0x11070 => UseCategory::H,
        0x11047..=0x1104D => UseCategory::GB,
        0x11052..=0x1106F => UseCategory::N,
        0x11071..=0x11072 => UseCategory::IV,
        0x11075 => UseCategory::B,

        // --- Sharada (U+11180..U+111DF) --------------------------
        // Historical script for Kashmiri / Sanskrit (8th century);
        // still used liturgically in Kashmiri Hindu communities.
        // 11180 candrabindu (M), 11181 anusvara (M), 11182 visarga (FM),
        // 11183..11191 independent vowels (IV),
        // 11192..111B2 consonants (B),
        // 111B3..111BF dependent vowel signs,
        // 111C0 virama (H),
        // 111C1..111C3 sign avagraha / aum / siddham (B/GB),
        // 111C4 letter om (B), 111C5..111C8 punctuation (GB),
        // 111C9 sandhi mark (M), 111CA nukta (M — below),
        // 111CB vowel modifier mark (M), 111CC extra short vowel mark (M),
        // 111CD sutra mark (M), 111CE sign vowel modifier (M),
        // 111CF sign inverted candrabindu (M),
        // 111D0..111D9 digits (N),
        // 111DA letter ekam (B),
        // 111DB..111DF punctuation (GB).
        0x11180..=0x11181 => UseCategory::M,
        0x11182 => UseCategory::FM,
        0x11183..=0x11191 => UseCategory::IV,
        0x11192..=0x111B2 => UseCategory::B,
        0x111B3 | 0x111BB..=0x111BF => UseCategory::VPst,
        // Sign-i (U+111B4) renders visually before the base in
        // Sharada despite Unicode marking it Top — the font ships
        // sign-i as a spacing pre-base glyph and the USE reorder
        // pass moves it to the head of the syllable. Sign-ii
        // (U+111B5) follows the same pattern in this font.
        0x111B4..=0x111B5 => UseCategory::VPre,
        0x111B6..=0x111BA => UseCategory::VBlw,
        0x111C0 => UseCategory::H,
        0x111C1..=0x111C4 => UseCategory::B,
        0x111C5..=0x111C8 | 0x111CD => UseCategory::GB,
        0x111C9..=0x111CC | 0x111CE..=0x111CF => UseCategory::M,
        0x111D0..=0x111D9 => UseCategory::N,
        0x111DA => UseCategory::B,
        0x111DB..=0x111DF => UseCategory::GB,

        // --- Khojki (U+11200..U+1124F) ---------------------------
        // Historical script for Sindhi / Khoja Ismaili community.
        // 11200..11211: 11200..11211 letters (mix of IV at start,
        //   then B). 11200 letter a (IV), 11201..11202 aa/i (IV),
        //   11203..11211 mostly consonants. We approximate by
        //   classifying 11200..11207 as IV (vowel letters) and
        //   11208..11211 as B (consonants) — Unicode UCD splits at
        //   11208 letter ka.
        // 11213..1122B consonants (B),
        // 1122C..11233 vowel signs (mix),
        // 11234 anusvara (M), 11235 virama (H),
        // 11236 nukta (M), 11237 shadda (M),
        // 11238..1123D punctuation (GB),
        // 1123E sign sukun (M), 1123F letter qa (B).
        0x11200..=0x11207 => UseCategory::IV,
        0x11208..=0x11211 | 0x11213..=0x1122B | 0x1123F => UseCategory::B,
        0x1122C..=0x1122E | 0x11232..=0x11233 => UseCategory::VPst,
        0x1122F => UseCategory::VBlw,
        0x11230..=0x11231 => UseCategory::VAbv,
        0x11234 | 0x11236..=0x11237 | 0x1123E => UseCategory::M,
        0x11235 => UseCategory::H,
        0x11238..=0x1123D => UseCategory::GB,

        // --- Tirhuta (U+11480..U+114DF) --------------------------
        // Historical script for Maithili / Sanskrit.
        // 11480..11489 independent vowels (IV),
        // 1148A..114AF consonants (B),
        // 114B0..114BE dependent vowel signs (mix),
        // 114BF candrabindu (M), 114C0 anusvara (M),
        // 114C1 visarga (FM), 114C2 virama (H),
        // 114C3 nukta (M),
        // 114C4..114C5 marks (M),
        // 114C6 abbreviation sign (GB), 114C7 om (B),
        // 114D0..114D9 digits (N).
        0x11480..=0x11489 => UseCategory::IV,
        0x1148A..=0x114AF | 0x114C7 => UseCategory::B,
        0x114B0..=0x114B2 | 0x114BB | 0x114BD..=0x114BE => UseCategory::VPst,
        0x114B3..=0x114B8 => UseCategory::VBlw,
        0x114B9 | 0x114BC => UseCategory::VPre,
        0x114BA => UseCategory::VAbv,
        0x114BF..=0x114C0 => UseCategory::M,
        0x114C1 => UseCategory::FM,
        0x114C2 => UseCategory::H,
        0x114C3..=0x114C5 => UseCategory::M,
        0x114C6 => UseCategory::GB,
        0x114D0..=0x114D9 => UseCategory::N,

        // --- Modi (U+11600..U+1165F) -----------------------------
        // Historical script for Marathi (17th century).
        // 11600..1162F: 11600..11605 independent vowels,
        //   11606..1162F consonants. (Modi has fewer vowels than
        //   Devanagari; 6 IV letters then 42 consonants.)
        // 11630..11640 dependent vowel signs / marks (mix),
        // 11641..11643 punctuation (GB? — actually digits).
        // Wait: 11641 digit zero ... no, Modi digits are 11650..11659.
        // 11641..11643 is unassigned in 15.0; 11644 is letter qa (B).
        // 11650..11659 digits (N), 1165D..1165F punctuation (GB).
        0x11600..=0x11605 => UseCategory::IV,
        0x11606..=0x1162F | 0x11644 => UseCategory::B,
        0x11630..=0x11632 | 0x1163B..=0x1163C => UseCategory::VPst,
        0x11633..=0x11638 => UseCategory::VBlw,
        0x11639..=0x1163A => UseCategory::VAbv,
        0x1163D | 0x11640 => UseCategory::M,
        0x1163E => UseCategory::FM,
        0x1163F => UseCategory::H,
        0x11650..=0x11659 => UseCategory::N,
        0x1165D..=0x1165F => UseCategory::GB,

        // --- Hangul Jamo (U+1100..U+11FF) + Extensions -----------
        // Leading consonants (Choseong): 1100..115F + A960..A97C.
        // Vowels (Jungseong): 1160..11A7 + D7B0..D7C6.
        // Trailing consonants (Jongseong): 11A8..11FF + D7CB..D7FB.
        //
        // All three categories map to B under USE — the state
        // machine treats them as stackable bases, and the font's
        // ljmo/vjmo/tjmo features pick the correct variant form per
        // position. The segmenter emits one syllable per L (+V+T),
        // matching HarfBuzz.
        0x1100..=0x115F | 0xA960..=0xA97C => UseCategory::B,
        0x1160..=0x11A7 | 0xD7B0..=0xD7C6 => UseCategory::B,
        0x11A8..=0x11FF | 0xD7CB..=0xD7FB => UseCategory::B,
        // Precomposed Hangul syllables are single bases.
        0xAC00..=0xD7A3 => UseCategory::B,

        _ => UseCategory::O,
    }
}

/// Returns `true` if the codepoint is a Hangul Leading Jamo
/// (Choseong). The USE segmenter uses this to anchor a Hangul syllable
/// — L is the required opening of `L V? T?`.
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
mod tests {
    use super::*;

    #[test]
    fn khmer_consonant_ka_is_base() {
        assert_eq!(use_category('\u{1780}'), UseCategory::B);
        assert_eq!(use_category('\u{17A2}'), UseCategory::B); // ha
    }

    #[test]
    fn khmer_independent_vowels_are_iv() {
        assert_eq!(use_category('\u{17A5}'), UseCategory::IV);
        assert_eq!(use_category('\u{17B3}'), UseCategory::IV);
    }

    #[test]
    fn khmer_coeng_is_halant() {
        assert_eq!(use_category('\u{17D2}'), UseCategory::H);
    }

    #[test]
    fn khmer_pre_base_vowels() {
        // U+17C1 sign e, U+17C2 sign ai, U+17C3 sign am-prefix.
        assert_eq!(use_category('\u{17C1}'), UseCategory::VPre);
        assert_eq!(use_category('\u{17C2}'), UseCategory::VPre);
        assert_eq!(use_category('\u{17C3}'), UseCategory::VPre);
        assert_eq!(use_position('\u{17C1}'), UsePosition::PreBase);
    }

    #[test]
    fn khmer_above_base_vowels() {
        assert_eq!(use_category('\u{17B7}'), UseCategory::VAbv); // i
        assert_eq!(use_category('\u{17B8}'), UseCategory::VAbv); // ii
        assert_eq!(use_position('\u{17B7}'), UsePosition::AboveBase);
    }

    #[test]
    fn khmer_below_base_vowels() {
        assert_eq!(use_category('\u{17BB}'), UseCategory::VBlw); // u
        assert_eq!(use_category('\u{17BC}'), UseCategory::VBlw); // uu
        assert_eq!(use_position('\u{17BB}'), UsePosition::BelowBase);
    }

    #[test]
    fn khmer_post_base_aa_and_au() {
        assert_eq!(use_category('\u{17B6}'), UseCategory::VPst);
        assert_eq!(use_category('\u{17C4}'), UseCategory::VPst);
        assert_eq!(use_position('\u{17B6}'), UsePosition::PostBase);
    }

    #[test]
    fn khmer_nikahit_and_reahmuk_are_final_marks() {
        assert_eq!(use_category('\u{17C6}'), UseCategory::FM);
        assert_eq!(use_category('\u{17C7}'), UseCategory::FM);
    }

    #[test]
    fn khmer_register_shifters_are_modifiers() {
        // U+17C9 muusikatoan, U+17CA triisap — register shifters.
        assert_eq!(use_category('\u{17C9}'), UseCategory::M);
        assert_eq!(use_category('\u{17CA}'), UseCategory::M);
    }

    #[test]
    fn khmer_digits_are_numbers() {
        assert_eq!(use_category('\u{17E0}'), UseCategory::N);
        assert_eq!(use_category('\u{17E9}'), UseCategory::N);
    }

    #[test]
    fn khmer_symbols_block_is_generic_base() {
        assert_eq!(use_category('\u{19E0}'), UseCategory::GB);
        assert_eq!(use_category('\u{19FF}'), UseCategory::GB);
    }

    #[test]
    fn format_characters_classify_correctly() {
        assert_eq!(use_category('\u{200C}'), UseCategory::ZWNJ);
        assert_eq!(use_category('\u{200D}'), UseCategory::ZWJ);
        assert_eq!(use_category('\u{FE00}'), UseCategory::VS);
    }

    #[test]
    fn dotted_circle_is_generic_base() {
        assert_eq!(use_category('\u{25CC}'), UseCategory::GB);
    }

    #[test]
    fn latin_and_other_scripts_fall_through_to_other() {
        assert_eq!(use_category('A'), UseCategory::O);
        assert_eq!(use_category('\u{0915}'), UseCategory::O); // Devanagari
    }

    #[test]
    fn positional_defaults_to_not_applicable_for_bases() {
        assert_eq!(use_position('\u{1780}'), UsePosition::NotApplicable);
        assert_eq!(use_position('A'), UsePosition::NotApplicable);
    }

    // --- Myanmar tests -------------------------------------------

    #[test]
    fn myanmar_consonants_are_bases() {
        assert_eq!(use_category('\u{1000}'), UseCategory::B); // ka
        assert_eq!(use_category('\u{1020}'), UseCategory::B); // la
        assert_eq!(use_category('\u{103F}'), UseCategory::B); // great sa
    }

    #[test]
    fn myanmar_independent_vowels() {
        assert_eq!(use_category('\u{1021}'), UseCategory::IV); // a
        assert_eq!(use_category('\u{1027}'), UseCategory::IV); // e
        assert_eq!(use_category('\u{102A}'), UseCategory::IV); // aw
    }

    #[test]
    fn myanmar_vowel_signs_positions() {
        // Post: 102B tall aa, 102C aa.
        assert_eq!(use_category('\u{102B}'), UseCategory::VPst);
        assert_eq!(use_category('\u{102C}'), UseCategory::VPst);
        // Above: 102D i, 102E ii, 1032 ai.
        assert_eq!(use_category('\u{102D}'), UseCategory::VAbv);
        assert_eq!(use_category('\u{102E}'), UseCategory::VAbv);
        assert_eq!(use_category('\u{1032}'), UseCategory::VAbv);
        // Below: 102F u, 1030 uu.
        assert_eq!(use_category('\u{102F}'), UseCategory::VBlw);
        assert_eq!(use_category('\u{1030}'), UseCategory::VBlw);
        // Pre: 1031 e (only pre-base vowel sign in Myanmar).
        assert_eq!(use_category('\u{1031}'), UseCategory::VPre);
        assert_eq!(use_position('\u{1031}'), UsePosition::PreBase);
    }

    #[test]
    fn myanmar_virama_and_asat_are_halant() {
        assert_eq!(use_category('\u{1039}'), UseCategory::H); // virama
        assert_eq!(use_category('\u{103A}'), UseCategory::H); // asat
    }

    #[test]
    fn myanmar_medial_consonants_are_cm() {
        assert_eq!(use_category('\u{103B}'), UseCategory::CM); // medial ya
        assert_eq!(use_category('\u{103C}'), UseCategory::CM); // medial ra
        assert_eq!(use_category('\u{103D}'), UseCategory::CM); // medial wa
        assert_eq!(use_category('\u{103E}'), UseCategory::CM); // medial ha
    }

    #[test]
    fn myanmar_final_marks() {
        assert_eq!(use_category('\u{1036}'), UseCategory::FM); // anusvara
        assert_eq!(use_category('\u{1037}'), UseCategory::FM); // dot below
        assert_eq!(use_category('\u{1038}'), UseCategory::FM); // visarga
    }

    #[test]
    fn myanmar_digits_are_numbers() {
        assert_eq!(use_category('\u{1040}'), UseCategory::N);
        assert_eq!(use_category('\u{1049}'), UseCategory::N);
    }

    #[test]
    fn myanmar_extended_a_consonants() {
        assert_eq!(use_category('\u{AA60}'), UseCategory::B); // shan letter kha
        assert_eq!(use_category('\u{AA7C}'), UseCategory::VAbv); // aiton ai
    }

    #[test]
    fn myanmar_extended_b_digits() {
        assert_eq!(use_category('\u{A9F0}'), UseCategory::N);
        assert_eq!(use_category('\u{A9F9}'), UseCategory::N);
    }

    // --- Thai tests ----------------------------------------------

    #[test]
    fn thai_consonants_are_bases() {
        assert_eq!(use_category('\u{0E01}'), UseCategory::B); // ko kai
        assert_eq!(use_category('\u{0E2E}'), UseCategory::B); // ho nokhuk
    }

    #[test]
    fn thai_pre_base_vowels() {
        // Sara e (0E40), sara ae (0E41), sara o (0E42),
        // sara ai-maimuan (0E43), sara ai-maimalai (0E44).
        assert_eq!(use_category('\u{0E40}'), UseCategory::VPre);
        assert_eq!(use_category('\u{0E44}'), UseCategory::VPre);
        assert_eq!(use_position('\u{0E40}'), UsePosition::PreBase);
    }

    #[test]
    fn thai_above_and_below_vowels() {
        // Mai han-akat (0E31), sara i (0E34), sara ii (0E35),
        // sara ue (0E36), sara uee (0E37).
        assert_eq!(use_category('\u{0E31}'), UseCategory::VAbv);
        assert_eq!(use_category('\u{0E34}'), UseCategory::VAbv);
        // Sara u (0E38), sara uu (0E39), phinthu (0E3A).
        assert_eq!(use_category('\u{0E38}'), UseCategory::VBlw);
        assert_eq!(use_category('\u{0E39}'), UseCategory::VBlw);
    }

    #[test]
    fn thai_tone_marks_are_modifiers() {
        // Mai taikhu (0E47), mai ek (0E48), mai tho (0E49),
        // thanthakhat (0E4C), nikkhahit (0E4D), yamakkan (0E4E).
        assert_eq!(use_category('\u{0E47}'), UseCategory::M);
        assert_eq!(use_category('\u{0E48}'), UseCategory::M);
        assert_eq!(use_category('\u{0E4C}'), UseCategory::M);
        assert_eq!(use_category('\u{0E4D}'), UseCategory::M);
    }

    #[test]
    fn thai_digits() {
        assert_eq!(use_category('\u{0E50}'), UseCategory::N);
        assert_eq!(use_category('\u{0E59}'), UseCategory::N);
    }

    // --- Lao tests -----------------------------------------------

    #[test]
    fn lao_consonants_are_bases() {
        assert_eq!(use_category('\u{0E81}'), UseCategory::B); // ko
        assert_eq!(use_category('\u{0E97}'), UseCategory::B); // tho
    }

    #[test]
    fn lao_vowels_positions() {
        assert_eq!(use_category('\u{0EB1}'), UseCategory::VAbv); // mai kan
        assert_eq!(use_category('\u{0EB8}'), UseCategory::VBlw); // sara u
        assert_eq!(use_category('\u{0EC0}'), UseCategory::VPre); // sara e
        assert_eq!(use_category('\u{0EB2}'), UseCategory::VPst); // sara aa
    }

    #[test]
    fn lao_tone_marks() {
        assert_eq!(use_category('\u{0EC8}'), UseCategory::M);
        assert_eq!(use_category('\u{0ECD}'), UseCategory::M);
    }

    #[test]
    fn lao_digits() {
        assert_eq!(use_category('\u{0ED0}'), UseCategory::N);
        assert_eq!(use_category('\u{0ED9}'), UseCategory::N);
    }

    // --- Hangul tests --------------------------------------------

    #[test]
    fn hangul_jamo_are_bases() {
        // Leading, vowel, trailing — all three classify as B for
        // the USE state machine; the font's ljmo/vjmo/tjmo features
        // pick the positional variant.
        assert_eq!(use_category('\u{1100}'), UseCategory::B); // L kiyeok
        assert_eq!(use_category('\u{1161}'), UseCategory::B); // V a
        assert_eq!(use_category('\u{11A8}'), UseCategory::B); // T kiyeok
    }

    #[test]
    fn hangul_jamo_predicates_split_l_v_t() {
        assert!(is_hangul_l('\u{1100}'));
        assert!(!is_hangul_l('\u{1161}'));
        assert!(is_hangul_v('\u{1161}'));
        assert!(!is_hangul_v('\u{11A8}'));
        assert!(is_hangul_t('\u{11A8}'));
        assert!(!is_hangul_t('\u{1100}'));
    }

    #[test]
    fn hangul_jamo_extensions_covered() {
        // Extended-A is all L. Extended-B has a mix — 0xD7B0..0xD7C6
        // are V, 0xD7CB..0xD7FB are T.
        assert!(is_hangul_l('\u{A960}'));
        assert!(is_hangul_v('\u{D7B0}'));
        assert!(is_hangul_t('\u{D7CB}'));
    }

    #[test]
    fn precomposed_hangul_syllable_is_base() {
        assert_eq!(use_category('\u{AC00}'), UseCategory::B); // 가
        assert_eq!(use_category('\u{D7A3}'), UseCategory::B);
    }

    // --- N'Ko tests ----------------------------------------------

    #[test]
    fn nko_letters_and_marks() {
        assert_eq!(use_category('\u{07CA}'), UseCategory::B); // letter a
        assert_eq!(use_category('\u{07EA}'), UseCategory::B);
        assert_eq!(use_category('\u{07EB}'), UseCategory::M); // tone mark
        assert_eq!(use_category('\u{07FD}'), UseCategory::M); // dantayalan
        assert_eq!(use_category('\u{07C0}'), UseCategory::N); // digit 0
        assert_eq!(use_category('\u{07C9}'), UseCategory::N); // digit 9
        assert_eq!(use_category('\u{07FA}'), UseCategory::CM); // lajanyalan
    }

    // --- Buginese tests ------------------------------------------

    #[test]
    fn buginese_letters_and_signs() {
        assert_eq!(use_category('\u{1A00}'), UseCategory::B); // ka
        assert_eq!(use_category('\u{1A16}'), UseCategory::B);
        assert_eq!(use_category('\u{1A17}'), UseCategory::VAbv); // sara i
        assert_eq!(use_category('\u{1A18}'), UseCategory::VBlw); // sara u
        assert_eq!(use_category('\u{1A19}'), UseCategory::VPre); // sara e
        assert_eq!(use_category('\u{1A1A}'), UseCategory::VPst); // sara o
        assert_eq!(use_category('\u{1A1B}'), UseCategory::VAbv); // sara ae
    }

    // --- Tai Tham tests ------------------------------------------

    #[test]
    fn tai_tham_basics() {
        assert_eq!(use_category('\u{1A20}'), UseCategory::B); // high ka
        assert_eq!(use_category('\u{1A4D}'), UseCategory::IV);
        assert_eq!(use_category('\u{1A60}'), UseCategory::H); // sakot
        assert_eq!(use_category('\u{1A55}'), UseCategory::CM); // medial ra
        assert_eq!(use_category('\u{1A6E}'), UseCategory::VPre); // pre-base
        assert_eq!(use_category('\u{1A80}'), UseCategory::N); // hora digit 0
    }

    // --- Balinese tests ------------------------------------------

    #[test]
    fn balinese_basics() {
        assert_eq!(use_category('\u{1B05}'), UseCategory::B); // letter a
        assert_eq!(use_category('\u{1B35}'), UseCategory::VPst); // tedung
        assert_eq!(use_category('\u{1B36}'), UseCategory::VAbv); // i
        assert_eq!(use_category('\u{1B39}'), UseCategory::VBlw); // u-style
        assert_eq!(use_category('\u{1B44}'), UseCategory::H); // adeg adeg
        assert_eq!(use_category('\u{1B50}'), UseCategory::N); // digit 0
    }

    // --- Sundanese tests -----------------------------------------

    #[test]
    fn sundanese_basics() {
        assert_eq!(use_category('\u{1B83}'), UseCategory::IV); // letter a
        assert_eq!(use_category('\u{1B95}'), UseCategory::B); // letter ka
        assert_eq!(use_category('\u{1B80}'), UseCategory::M); // panyecek
        assert_eq!(use_category('\u{1B82}'), UseCategory::FM); // pangwisad
        assert_eq!(use_category('\u{1BA4}'), UseCategory::VAbv); // sara i
        assert_eq!(use_category('\u{1BA6}'), UseCategory::VPre); // sara e
        assert_eq!(use_category('\u{1BAB}'), UseCategory::H); // virama
        assert_eq!(use_category('\u{1BB0}'), UseCategory::N); // digit 0
    }

    // --- Lepcha tests --------------------------------------------

    #[test]
    fn lepcha_basics() {
        assert_eq!(use_category('\u{1C00}'), UseCategory::B); // ka
        assert_eq!(use_category('\u{1C24}'), UseCategory::CM); // subjoined ya
        assert_eq!(use_category('\u{1C26}'), UseCategory::VPst); // sign i
        assert_eq!(use_category('\u{1C27}'), UseCategory::VPre); // sign o
        assert_eq!(use_category('\u{1C36}'), UseCategory::M); // ran
        assert_eq!(use_category('\u{1C40}'), UseCategory::N); // digit 0
    }

    // --- Limbu tests ---------------------------------------------

    #[test]
    fn limbu_basics() {
        assert_eq!(use_category('\u{1900}'), UseCategory::B); // letter ka
        assert_eq!(use_category('\u{1920}'), UseCategory::VAbv); // sign a
        assert_eq!(use_category('\u{1923}'), UseCategory::VBlw); // sign ee
        assert_eq!(use_category('\u{1929}'), UseCategory::CM); // subjoined ya
        assert_eq!(use_category('\u{1930}'), UseCategory::CM); // small ka
        assert_eq!(use_category('\u{1939}'), UseCategory::M); // tone marker
        assert_eq!(use_category('\u{1946}'), UseCategory::N); // digit 0
    }

    // --- Cham tests ----------------------------------------------

    #[test]
    fn cham_basics() {
        assert_eq!(use_category('\u{AA00}'), UseCategory::IV); // letter a
        assert_eq!(use_category('\u{AA06}'), UseCategory::B);
        assert_eq!(use_category('\u{AA29}'), UseCategory::VAbv); // sign aa
        assert_eq!(use_category('\u{AA2F}'), UseCategory::VPre); // sign oe (pre-base)
        assert_eq!(use_category('\u{AA34}'), UseCategory::CM); // medial ra
        assert_eq!(use_category('\u{AA40}'), UseCategory::CM); // final k
        assert_eq!(use_category('\u{AA43}'), UseCategory::FM); // final ng
        assert_eq!(use_category('\u{AA50}'), UseCategory::N); // digit 0
    }

    // --- Brahmi-family historical-script tests -------------------

    #[test]
    fn brahmi_basics() {
        // 11000 candrabindu (M), 11001 anusvara (M), 11002 visarga (FM).
        assert_eq!(use_category('\u{11000}'), UseCategory::M);
        assert_eq!(use_category('\u{11001}'), UseCategory::M);
        assert_eq!(use_category('\u{11002}'), UseCategory::FM);
        // Independent vowels.
        assert_eq!(use_category('\u{11003}'), UseCategory::IV);
        assert_eq!(use_category('\u{11005}'), UseCategory::IV);
        // Letter ka.
        assert_eq!(use_category('\u{11015}'), UseCategory::B);
        // Vowel signs.
        assert_eq!(use_category('\u{11038}'), UseCategory::VPst); // sign aa
        assert_eq!(use_category('\u{11039}'), UseCategory::VAbv); // sign i
        assert_eq!(use_category('\u{1103B}'), UseCategory::VBlw); // sign u
        // Virama.
        assert_eq!(use_category('\u{11046}'), UseCategory::H);
        // Digits.
        assert_eq!(use_category('\u{11066}'), UseCategory::N);
        assert_eq!(use_category('\u{1106F}'), UseCategory::N);
    }

    #[test]
    fn sharada_basics() {
        // 11180 candrabindu (M), 11181 anusvara (M), 11182 visarga (FM).
        assert_eq!(use_category('\u{11180}'), UseCategory::M);
        assert_eq!(use_category('\u{11181}'), UseCategory::M);
        assert_eq!(use_category('\u{11182}'), UseCategory::FM);
        // Letters.
        assert_eq!(use_category('\u{11183}'), UseCategory::IV); // letter a
        assert_eq!(use_category('\u{11192}'), UseCategory::B); // letter ka
        // Vowel signs. Sign-i renders visually before the base in
        // Sharada (font has spacing pre-base sign-i glyph), so the
        // USE category is VPre to drive the pre-base reorder.
        assert_eq!(use_category('\u{111B3}'), UseCategory::VPst); // sign aa
        assert_eq!(use_category('\u{111B4}'), UseCategory::VPre); // sign i (pre-base)
        assert_eq!(use_position('\u{111B4}'), UsePosition::PreBase);
        assert_eq!(use_category('\u{111B6}'), UseCategory::VBlw); // sign u
        // Virama.
        assert_eq!(use_category('\u{111C0}'), UseCategory::H);
        // Digits.
        assert_eq!(use_category('\u{111D0}'), UseCategory::N);
        assert_eq!(use_category('\u{111D9}'), UseCategory::N);
    }

    #[test]
    fn khojki_basics() {
        // Letters.
        assert_eq!(use_category('\u{11200}'), UseCategory::IV); // letter a
        assert_eq!(use_category('\u{11208}'), UseCategory::B); // letter ka
        // Vowel signs.
        assert_eq!(use_category('\u{1122C}'), UseCategory::VPst); // sign aa
        assert_eq!(use_category('\u{11230}'), UseCategory::VAbv); // sign e
        assert_eq!(use_category('\u{1122F}'), UseCategory::VBlw); // sign u
        // Anusvara / virama.
        assert_eq!(use_category('\u{11234}'), UseCategory::M); // anusvara
        assert_eq!(use_category('\u{11235}'), UseCategory::H); // virama
        // Letter qa.
        assert_eq!(use_category('\u{1123F}'), UseCategory::B);
    }

    #[test]
    fn tirhuta_basics() {
        // Letters.
        assert_eq!(use_category('\u{11480}'), UseCategory::IV); // letter a
        assert_eq!(use_category('\u{1148A}'), UseCategory::B); // letter ka
        // Vowel signs.
        assert_eq!(use_category('\u{114B0}'), UseCategory::VPst); // sign aa
        assert_eq!(use_category('\u{114B3}'), UseCategory::VBlw); // sign u
        assert_eq!(use_category('\u{114B9}'), UseCategory::VPre); // sign e (pre-base)
        assert_eq!(use_category('\u{114BC}'), UseCategory::VPre); // sign o (pre-base)
        assert_eq!(use_category('\u{114BA}'), UseCategory::VAbv); // sign short e
        // Marks / virama.
        assert_eq!(use_category('\u{114C0}'), UseCategory::M); // anusvara
        assert_eq!(use_category('\u{114C1}'), UseCategory::FM); // visarga
        assert_eq!(use_category('\u{114C2}'), UseCategory::H); // virama
        // Digits.
        assert_eq!(use_category('\u{114D0}'), UseCategory::N);
    }

    #[test]
    fn modi_basics() {
        // Letters.
        assert_eq!(use_category('\u{11600}'), UseCategory::IV); // letter a
        assert_eq!(use_category('\u{11606}'), UseCategory::B); // letter ka
        // Vowel signs.
        assert_eq!(use_category('\u{11630}'), UseCategory::VPst); // sign aa
        assert_eq!(use_category('\u{11633}'), UseCategory::VBlw); // sign u
        assert_eq!(use_category('\u{11639}'), UseCategory::VAbv); // sign e
        // Marks / virama.
        assert_eq!(use_category('\u{1163D}'), UseCategory::M); // anusvara
        assert_eq!(use_category('\u{1163E}'), UseCategory::FM); // visarga
        assert_eq!(use_category('\u{1163F}'), UseCategory::H); // virama
        // Digits.
        assert_eq!(use_category('\u{11650}'), UseCategory::N);
        assert_eq!(use_category('\u{11659}'), UseCategory::N);
    }
}
