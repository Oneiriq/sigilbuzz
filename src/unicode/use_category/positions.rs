//! USE positional category table: the `use_position` lookup, one
//! match arm group per script block.

use super::UsePosition;

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
        //   positional role in the USE tables. They attach as
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
        // Medial ra (U+103C) renders before the base in Myanmar:
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
        // (07FD) sits below. No pre-base or post-base vowels: N'Ko
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
        // 1A60 sakot (halant, NotApplicable for position),
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
        // 1B3F sign le tedung (pre-base: sign le + tedung
        //   composed; renders before the base then a tedung after),
        // 1B40 sign taa-le (pre-base, like Devanagari sign O),
        // 1B41 sign taa-le tedung (pre-base),
        // 1B42 above (sign ie), 1B43 above (sign ai),
        // 1B44 adeg adeg (halant, NotApplicable),
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
        // AA2F..AA30 pre-base (oe/o: render visually before the
        //   base consonant; the IndicPositionalCategory column
        //   marks them Top_And_Left in the Unicode Standard, but
        //   the USE places them in the pre-base bucket, same as
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
        0x11000..=0x11001 | 0x11039..=0x1103A | 0x11041..=0x11042 | 0x11073 => {
            UsePosition::AboveBase
        }
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
        // 1122C..1122E vowel signs (post: sign aa/i/ii),
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
        // 114B9 sign e (pre-base: Tirhuta places sign-e visually
        //   before the base, like Bengali sign-e),
        // 114BA sign short e (above),
        // 114BB sign ai (post), 114BC sign o (pre, like sign-e),
        // 114BD sign short o (post), 114BE sign au (post),
        // 114BF sign candrabindu (above), 114C0 sign anusvara (above),
        // 114C1 sign visarga (post), 114C2 sign virama (NotApplicable),
        // 114C3 sign nukta (below).
        0x114BA | 0x114BF..=0x114C0 => UsePosition::AboveBase,
        0x114B3..=0x114B8 | 0x114C3 => UsePosition::BelowBase,
        0x114B9 | 0x114BC => UsePosition::PreBase,
        0x114B0..=0x114B2 | 0x114BB | 0x114BD..=0x114BE | 0x114C1 => UsePosition::PostBase,

        // --- Modi (U+11600..U+1165F) -----------------------------
        // 11630..11632 vowel signs (post: sign aa/i/ii),
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

        // Currency + signs: no positional role (the currency is a
        // base glyph itself).
        _ => UsePosition::NotApplicable,
    }
}
