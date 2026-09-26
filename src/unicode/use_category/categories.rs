//! USE syllabic category table: the `use_category` lookup, one match
//! arm group per script block.

use super::UseCategory;

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
/// Arms are grouped by script / Unicode block. `match_same_arms` is
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
        // Anusvara, dot below, visarga: syllable-final marks.
        0x1036..=0x1038 => UseCategory::FM,
        // Virama (U+1039) + asat (U+103A). asat is the "explicit
        // virama" that doesn't trigger subjoining; classifying it as
        // H lets the state machine end the syllable cleanly.
        0x1039..=0x103A => UseCategory::H,
        // Medial consonants: ya (103B), ra (103C), wa (103D), ha
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
        // Myanmar medial mon la (1082): consonant modifier.
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
        // (VAbv), letter sign (0xA9E6, modifier), more consonants
        // 0xA9E7..0xA9EF, Shan digits 0xA9F0..0xA9F9, consonants
        // 0xA9FA..0xA9FE, reserved 0xA9FF.
        0xA9E0..=0xA9E4 | 0xA9E7..=0xA9EF | 0xA9FA..=0xA9FE => UseCategory::B,
        0xA9E5 => UseCategory::VAbv,
        0xA9E6 => UseCategory::CM,
        0xA9F0..=0xA9F9 => UseCategory::N,

        // --- Thai (U+0E00..U+0E7F) -------------------------------
        // Consonants 0E01..0E2E (incl. ng, cho, phoom, ro, lo, wo,
        // so, ho, o, ng-obsolete). No explicit halant: Thai has no
        // subjoining.
        0x0E01..=0x0E2E => UseCategory::B,
        // Independent vowels 0E2F paiyannoi, 0E46 maiyamok (B-like
        // repeat mark). Treat 0E2F as GB (punctuation-style) and
        // 0E46 as GB. Both can stand alone.
        0x0E2F | 0x0E46 | 0x0E4F | 0x0E5A..=0x0E5B => UseCategory::GB,
        // Thai tonal / vowel placement.
        0x0E30 | 0x0E32 | 0x0E33 => UseCategory::VPst,
        0x0E31 | 0x0E34..=0x0E37 => UseCategory::VAbv,
        0x0E38..=0x0E39 => UseCategory::VBlw,
        // Pinthu (0E3A): silencer, above-base in Thai.
        0x0E3A => UseCategory::VBlw,
        // Pre-base vowels.
        0x0E40..=0x0E44 => UseCategory::VPre,
        // Thai currency signs (0E3F baht). GB.
        0x0E3F => UseCategory::GB,
        // Mai Taikhu / Mai Ek / Mai Tho / Mai Tri / Mai Chattawa /
        // Thanthakhat / Nikhahit / Yamakkan: tone / final marks.
        // 0E45 (lakkhangyao): long-vowel extender (VPst-ish).
        0x0E45 => UseCategory::VPst,
        0x0E47..=0x0E4E => UseCategory::M,
        // Thai digits.
        0x0E50..=0x0E59 => UseCategory::N,

        // --- Lao (U+0E80..U+0EFF) -------------------------------
        // Lao consonants (with gaps: 0E81, 0E82, 0E84, 0E86..0E8A,
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
        // 0EBA (sign pali virama): used as silencer, like pinthu.
        0x0EBA => UseCategory::VBlw,
        // Lao semivowels (0EBB..0EBC): above-base consonant-like
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
        // lajanyalan (07FA, consonant modifier / TATWEEL-like).
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
        // signs 1A58..1A5E (above modifiers, CM),
        // sa 1A5F (final consonant, CM),
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
        // 1B34 rerekan (above modifier, M),
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
        // 1B80 panyecek (anusvara, M),
        // 1B81 panglayar (M),
        // 1B82 pangwisad (FM),
        // 1B83..1B89 independent vowels,
        // 1B8A..1BA0 letters,
        // 1BA1 pamingkal (post-base medial, CM),
        // 1BA2..1BA3 panyakra/panyikuh (below medials, CM),
        // 1BA4 vowel-i (above), 1BA5 vowel-u (below),
        // 1BA6 vowel-e (pre), 1BA7 vowel-aa (post),
        // 1BA8..1BA9 vowel-eu/ae (above),
        // 1BAA pamaaeh (post, final mark / virama-equivalent).
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
        // 1C24..1C2B subjoined consonants (CM: they sit below or
        // post a base, attached via halant-like behavior),
        // 1C2C..1C2F vowel signs,
        // 1C30..1C33 vowel signs (post),
        // 1C34..1C35 consonant signs (post),
        // 1C36 ran (above tone), 1C37 nukta (below, M),
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
        // 1923..1928 below (ee/ai/oo/au/e/o, wait some are above),
        //   actually 1925..1926 above (oo/au), 1923..1924 below
        //   (ee/ai), 1927..1928 below (e/o).
        // 1929..192B subjoined (CM, below: yya/ra/sa subjoined),
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
        // AA29..AA2E vowel signs (above: aa/i/ii/ei/u),
        // AA2F..AA30 vowel signs (post: oe/o),
        // AA31..AA32 vowel signs (above: ai/au),
        // AA33 medial ya (post, CM),
        // AA34..AA36 medial ra/la/wa (below, CM),
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
        // 11000 candrabindu (M: above-base modifier),
        // 11001 anusvara (M),
        // 11002 visarga (FM: final mark),
        // 11003..11037: 11003..11005 independent vowels,
        //   11006..11037 consonants.
        // 11038..11045 dependent vowel signs (mix of above/below/post),
        // 11046 virama (H),
        // 11047..1104D punctuation (GB),
        // 11052..11065 number signs (N: Brahmi numeric system),
        // 11066..1106F digits (N),
        // 11070 old tamil virama (H: Pulli sign),
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
        // 111C9 sandhi mark (M), 111CA nukta (M, below),
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
        // Sharada despite Unicode marking it Top. The font ships
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
        //   11208..11211 as B (consonants). Unicode UCD splits at
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
        // 11641..11643 punctuation (GB? Actually digits).
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
        // All three categories map to B under USE. The state
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
