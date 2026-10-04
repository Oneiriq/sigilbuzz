//! ISO 15924 codes and writing directions for scripts.
//!
//! [`Script`] is a coarse bucket, so the mapping to ISO 15924 is
//! many-to-one in one direction: every bucket but [`Script::Other`] has
//! one canonical code, while a few codes fold into the same bucket
//! (`Hira`, `Kana`, and `Hans` all shape as [`Script::Han`]).
//!
//! The right-to-left list follows HarfBuzz's
//! `hb_script_get_horizontal_direction`, which rustybuzz 0.20 mirrors
//! as `Direction::from_script`.

use super::Script;
use crate::buffer::Direction;

/// Canonical ISO 15924 code for each bucket, in both directions.
const CODES: &[(Script, [u8; 4])] = &[
    (Script::Latin, *b"Latn"),
    (Script::Han, *b"Hani"),
    (Script::Arabic, *b"Arab"),
    (Script::Hebrew, *b"Hebr"),
    (Script::Cyrillic, *b"Cyrl"),
    (Script::Greek, *b"Grek"),
    (Script::Devanagari, *b"Deva"),
    (Script::Bengali, *b"Beng"),
    (Script::Gurmukhi, *b"Guru"),
    (Script::Gujarati, *b"Gujr"),
    (Script::Oriya, *b"Orya"),
    (Script::Tamil, *b"Taml"),
    (Script::Telugu, *b"Telu"),
    (Script::Kannada, *b"Knda"),
    (Script::Malayalam, *b"Mlym"),
    (Script::Sinhala, *b"Sinh"),
    (Script::Khmer, *b"Khmr"),
    (Script::Myanmar, *b"Mymr"),
    (Script::Thai, *b"Thai"),
    (Script::Lao, *b"Laoo"),
    (Script::Hangul, *b"Hang"),
    (Script::Tibetan, *b"Tibt"),
    (Script::Mongolian, *b"Mong"),
    (Script::NKo, *b"Nkoo"),
    (Script::Buginese, *b"Bugi"),
    (Script::TaiTham, *b"Lana"),
    (Script::Balinese, *b"Bali"),
    (Script::Sundanese, *b"Sund"),
    (Script::Lepcha, *b"Lepc"),
    (Script::Limbu, *b"Limb"),
    (Script::Cham, *b"Cham"),
    (Script::Brahmi, *b"Brah"),
    (Script::Sharada, *b"Shrd"),
    (Script::Khojki, *b"Khoj"),
    (Script::Tirhuta, *b"Tirh"),
    (Script::Modi, *b"Modi"),
];

/// A bucket, its ISO 15924 code, and the script tags its lookups try.
type TableCode = (Script, [u8; 4], [[u8; 4]; 2]);

/// The buckets added in 0.22.0, Syriac through Seal (see
/// [`super::Script`]): each with its ISO 15924 code and the script
/// tags its lookups try. HarfBuzz tags each of these scripts with
/// its ISO 15924 code in lowercase (`hb_ot_old_tag_from_script` in
/// `hb-ot-tag.cc`), and sigilbuzz falls back to `DFLT` as for the
/// other buckets.
const TABLE_CODES: &[TableCode] = &[
    (Script::Syriac, *b"Syrc", [*b"syrc", *b"DFLT"]),
    (Script::Buhid, *b"Buhd", [*b"buhd", *b"DFLT"]),
    (Script::Hanunoo, *b"Hano", [*b"hano", *b"DFLT"]),
    (Script::Tagalog, *b"Tglg", [*b"tglg", *b"DFLT"]),
    (Script::Tagbanwa, *b"Tagb", [*b"tagb", *b"DFLT"]),
    (Script::TaiLe, *b"Tale", [*b"tale", *b"DFLT"]),
    (Script::Kharoshthi, *b"Khar", [*b"khar", *b"DFLT"]),
    (Script::SylotiNagri, *b"Sylo", [*b"sylo", *b"DFLT"]),
    (Script::Tifinagh, *b"Tfng", [*b"tfng", *b"DFLT"]),
    (Script::PhagsPa, *b"Phag", [*b"phag", *b"DFLT"]),
    (Script::KayahLi, *b"Kali", [*b"kali", *b"DFLT"]),
    (Script::Rejang, *b"Rjng", [*b"rjng", *b"DFLT"]),
    (Script::Saurashtra, *b"Saur", [*b"saur", *b"DFLT"]),
    (Script::EgyptianHieroglyphs, *b"Egyp", [*b"egyp", *b"DFLT"]),
    (Script::Javanese, *b"Java", [*b"java", *b"DFLT"]),
    (Script::Kaithi, *b"Kthi", [*b"kthi", *b"DFLT"]),
    (Script::MeeteiMayek, *b"Mtei", [*b"mtei", *b"DFLT"]),
    (Script::TaiViet, *b"Tavt", [*b"tavt", *b"DFLT"]),
    (Script::Batak, *b"Batk", [*b"batk", *b"DFLT"]),
    (Script::Mandaic, *b"Mand", [*b"mand", *b"DFLT"]),
    (Script::Chakma, *b"Cakm", [*b"cakm", *b"DFLT"]),
    (Script::Miao, *b"Plrd", [*b"plrd", *b"DFLT"]),
    (Script::Takri, *b"Takr", [*b"takr", *b"DFLT"]),
    (Script::Duployan, *b"Dupl", [*b"dupl", *b"DFLT"]),
    (Script::Grantha, *b"Gran", [*b"gran", *b"DFLT"]),
    (Script::Khudawadi, *b"Sind", [*b"sind", *b"DFLT"]),
    (Script::Mahajani, *b"Mahj", [*b"mahj", *b"DFLT"]),
    (Script::Manichaean, *b"Mani", [*b"mani", *b"DFLT"]),
    (Script::PahawhHmong, *b"Hmng", [*b"hmng", *b"DFLT"]),
    (Script::PsalterPahlavi, *b"Phlp", [*b"phlp", *b"DFLT"]),
    (Script::Siddham, *b"Sidd", [*b"sidd", *b"DFLT"]),
    (Script::Ahom, *b"Ahom", [*b"ahom", *b"DFLT"]),
    (Script::Multani, *b"Mult", [*b"mult", *b"DFLT"]),
    (Script::Adlam, *b"Adlm", [*b"adlm", *b"DFLT"]),
    (Script::Bhaiksuki, *b"Bhks", [*b"bhks", *b"DFLT"]),
    (Script::Marchen, *b"Marc", [*b"marc", *b"DFLT"]),
    (Script::Newa, *b"Newa", [*b"newa", *b"DFLT"]),
    (Script::MasaramGondi, *b"Gonm", [*b"gonm", *b"DFLT"]),
    (Script::Soyombo, *b"Soyo", [*b"soyo", *b"DFLT"]),
    (Script::ZanabazarSquare, *b"Zanb", [*b"zanb", *b"DFLT"]),
    (Script::Dogra, *b"Dogr", [*b"dogr", *b"DFLT"]),
    (Script::GunjalaGondi, *b"Gong", [*b"gong", *b"DFLT"]),
    (Script::HanifiRohingya, *b"Rohg", [*b"rohg", *b"DFLT"]),
    (Script::Makasar, *b"Maka", [*b"maka", *b"DFLT"]),
    (Script::Medefaidrin, *b"Medf", [*b"medf", *b"DFLT"]),
    (Script::OldSogdian, *b"Sogo", [*b"sogo", *b"DFLT"]),
    (Script::Sogdian, *b"Sogd", [*b"sogd", *b"DFLT"]),
    (Script::Elymaic, *b"Elym", [*b"elym", *b"DFLT"]),
    (Script::Nandinagari, *b"Nand", [*b"nand", *b"DFLT"]),
    (Script::NyiakengPuachueHmong, *b"Hmnp", [*b"hmnp", *b"DFLT"]),
    (Script::Wancho, *b"Wcho", [*b"wcho", *b"DFLT"]),
    (Script::Chorasmian, *b"Chrs", [*b"chrs", *b"DFLT"]),
    (Script::DivesAkuru, *b"Diak", [*b"diak", *b"DFLT"]),
    (Script::KhitanSmallScript, *b"Kits", [*b"kits", *b"DFLT"]),
    (Script::Yezidi, *b"Yezi", [*b"yezi", *b"DFLT"]),
    (Script::CyproMinoan, *b"Cpmn", [*b"cpmn", *b"DFLT"]),
    (Script::OldUyghur, *b"Ougr", [*b"ougr", *b"DFLT"]),
    (Script::Tangsa, *b"Tnsa", [*b"tnsa", *b"DFLT"]),
    (Script::Toto, *b"Toto", [*b"toto", *b"DFLT"]),
    (Script::Vithkuqi, *b"Vith", [*b"vith", *b"DFLT"]),
    (Script::Kawi, *b"Kawi", [*b"kawi", *b"DFLT"]),
    (Script::NagMundari, *b"Nagm", [*b"nagm", *b"DFLT"]),
    (Script::Garay, *b"Gara", [*b"gara", *b"DFLT"]),
    (Script::GurungKhema, *b"Gukh", [*b"gukh", *b"DFLT"]),
    (Script::KiratRai, *b"Krai", [*b"krai", *b"DFLT"]),
    (Script::OlOnal, *b"Onao", [*b"onao", *b"DFLT"]),
    (Script::Sunuwar, *b"Sunu", [*b"sunu", *b"DFLT"]),
    (Script::Todhri, *b"Todr", [*b"todr", *b"DFLT"]),
    (Script::TuluTigalari, *b"Tutg", [*b"tutg", *b"DFLT"]),
    (Script::BeriaErfe, *b"Berf", [*b"berf", *b"DFLT"]),
    (Script::Sidetic, *b"Sidt", [*b"sidt", *b"DFLT"]),
    (Script::TaiYo, *b"Tayo", [*b"tayo", *b"DFLT"]),
    (Script::TolongSiki, *b"Tols", [*b"tols", *b"DFLT"]),
    (Script::Jurchen, *b"Jurc", [*b"jurc", *b"DFLT"]),
    (Script::ProtoCuneiform, *b"Pcun", [*b"pcun", *b"DFLT"]),
    (Script::Seal, *b"Seal", [*b"seal", *b"DFLT"]),
];

/// Extra ISO 15924 codes that fold into an existing bucket.
const ALIASES: &[(&[u8; 4], Script)] = &[
    (b"Hans", Script::Han),
    (b"Hant", Script::Han),
    (b"Hira", Script::Han),
    (b"Kana", Script::Han),
    (b"Hrkt", Script::Han),
    (b"Jpan", Script::Han),
    (b"Kore", Script::Hangul),
    (b"Jamo", Script::Hangul),
];

/// Scripts HarfBuzz lays out right to left.
const RTL_CODES: &[&[u8; 4]] = &[
    b"Arab", b"Hebr", b"Syrc", b"Thaa", b"Cprt", b"Khar", b"Phnx", b"Nkoo", b"Lydi", b"Avst",
    b"Armi", b"Phli", b"Prti", b"Sarb", b"Orkh", b"Samr", b"Mand", b"Merc", b"Mero", b"Mani",
    b"Mend", b"Nbat", b"Narb", b"Palm", b"Phlp", b"Hatr", b"Adlm", b"Rohg", b"Sogo", b"Sogd",
    b"Elym", b"Chrs", b"Yezi", b"Ougr", b"Gara", b"Sidt",
];

/// Scripts written in either direction; HarfBuzz reports no default
/// for them (see harfbuzz/harfbuzz#1000).
const BIDIRECTIONAL_CODES: &[&[u8; 4]] = &[b"Hung", b"Ital", b"Runr", b"Tfng"];

/// Normalizes the case of an ISO 15924 code: first letter upper,
/// the rest lower (`arab` and `ARAB` both become `Arab`).
fn title_case(code: [u8; 4]) -> [u8; 4] {
    [
        code[0].to_ascii_uppercase(),
        code[1].to_ascii_lowercase(),
        code[2].to_ascii_lowercase(),
        code[3].to_ascii_lowercase(),
    ]
}

impl Script {
    /// The ISO 15924 code for this bucket, or `None` for
    /// [`Script::Other`].
    ///
    /// # Examples
    ///
    /// ```
    /// use sigilbuzz::UnicodeScript;
    ///
    /// assert_eq!(UnicodeScript::Arabic.iso15924_tag(), Some(*b"Arab"));
    /// assert_eq!(UnicodeScript::TaiTham.iso15924_tag(), Some(*b"Lana"));
    /// assert_eq!(UnicodeScript::Other.iso15924_tag(), None);
    /// ```
    #[must_use]
    pub fn iso15924_tag(self) -> Option<[u8; 4]> {
        CODES
            .iter()
            .find(|(s, _)| *s == self)
            .map(|(_, code)| *code)
            .or_else(|| {
                TABLE_CODES
                    .iter()
                    .find(|(s, _, _)| *s == self)
                    .map(|(_, code, _)| *code)
            })
    }

    /// The script tags the lookups of a bucket added in 0.22.0 try, or
    /// `None` for the older buckets, whose lists live with their
    /// shapers.
    pub(crate) fn table_script_priority(self) -> Option<&'static [[u8; 4]]> {
        TABLE_CODES
            .iter()
            .find(|(s, _, _)| *s == self)
            .map(|(_, _, tags)| &tags[..])
    }

    /// The bucket for an ISO 15924 code, matched case-insensitively.
    /// Returns `None` for codes sigilbuzz has no bucket for, including
    /// `Zyyy` (Common), `Zinh` (Inherited), and `Zzzz` (Unknown).
    ///
    /// # Examples
    ///
    /// ```
    /// use sigilbuzz::UnicodeScript;
    ///
    /// assert_eq!(UnicodeScript::from_iso15924_tag(*b"Cyrl"), Some(UnicodeScript::Cyrillic));
    /// assert_eq!(UnicodeScript::from_iso15924_tag(*b"hira"), Some(UnicodeScript::Han));
    /// assert_eq!(UnicodeScript::from_iso15924_tag(*b"Zyyy"), None);
    /// ```
    #[must_use]
    pub fn from_iso15924_tag(tag: [u8; 4]) -> Option<Self> {
        let tag = title_case(tag);
        CODES
            .iter()
            .find(|(_, code)| *code == tag)
            .map(|(s, _)| *s)
            .or_else(|| {
                TABLE_CODES
                    .iter()
                    .find(|(_, code, _)| *code == tag)
                    .map(|(s, _, _)| *s)
            })
            .or_else(|| {
                ALIASES
                    .iter()
                    .find(|(code, _)| **code == tag)
                    .map(|(_, s)| *s)
            })
    }

    /// The direction horizontal text in this script runs: right to
    /// left for Arabic, Hebrew, N'Ko, Syriac, Adlam, and the other
    /// right-to-left buckets, left to right otherwise
    /// (including [`Script::Other`]).
    ///
    /// # Examples
    ///
    /// ```
    /// use sigilbuzz::{Direction, UnicodeScript};
    ///
    /// assert_eq!(UnicodeScript::Hebrew.horizontal_direction(), Direction::Rtl);
    /// assert_eq!(UnicodeScript::Thai.horizontal_direction(), Direction::Ltr);
    /// ```
    #[must_use]
    pub fn horizontal_direction(self) -> Direction {
        self.iso15924_tag()
            .and_then(Direction::horizontal_for_script)
            .unwrap_or(Direction::Ltr)
    }
}

impl Direction {
    /// The horizontal direction of the script with ISO 15924 code
    /// `tag` (matched case-insensitively), following HarfBuzz's
    /// `hb_script_get_horizontal_direction`: [`Direction::Rtl`] for
    /// the right-to-left scripts (Arabic, Hebrew, Syriac, Thaana,
    /// N'Ko, Adlam, Hanifi Rohingya, and the historical Semitic and
    /// Iranian scripts), `None` for Old Hungarian, Old Italic, Runic,
    /// and Tifinagh (written either way), and [`Direction::Ltr`] for
    /// everything else, including unknown codes.
    ///
    /// # Examples
    ///
    /// ```
    /// use sigilbuzz::Direction;
    ///
    /// assert_eq!(Direction::horizontal_for_script(*b"Syrc"), Some(Direction::Rtl));
    /// assert_eq!(Direction::horizontal_for_script(*b"Latn"), Some(Direction::Ltr));
    /// assert_eq!(Direction::horizontal_for_script(*b"Runr"), None);
    /// ```
    #[must_use]
    pub fn horizontal_for_script(tag: [u8; 4]) -> Option<Self> {
        let tag = title_case(tag);
        if RTL_CODES.iter().any(|code| **code == tag) {
            Some(Self::Rtl)
        } else if BIDIRECTIONAL_CODES.iter().any(|code| **code == tag) {
            None
        } else {
            Some(Self::Ltr)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_bucket_but_other_round_trips() {
        for (script, code) in all_codes() {
            assert_eq!(script.iso15924_tag(), Some(code));
            assert_eq!(Script::from_iso15924_tag(code), Some(script));
        }
        assert_eq!(Script::Other.iso15924_tag(), None);
    }

    fn all_codes() -> impl Iterator<Item = (Script, [u8; 4])> {
        CODES
            .iter()
            .copied()
            .chain(TABLE_CODES.iter().map(|&(s, c, _)| (s, c)))
    }

    #[test]
    fn codes_are_unique() {
        let codes: alloc::vec::Vec<(Script, [u8; 4])> = all_codes().collect();
        for (i, (s, c)) in codes.iter().enumerate() {
            for (s2, c2) in &codes[i + 1..] {
                assert_ne!(s, s2);
                assert_ne!(c, c2);
            }
        }
    }

    #[test]
    fn table_scripts_try_their_lowercase_code_then_dflt() {
        // HarfBuzz's `hb_ot_old_tag_from_script`: the ISO 15924 code
        // with its first letter lowercased.
        for &(script, code, tags) in TABLE_CODES {
            assert_eq!(tags, [code.map(|b| b.to_ascii_lowercase()), *b"DFLT"]);
            assert_eq!(script.table_script_priority(), Some(&tags[..]));
        }
        assert_eq!(
            Script::Javanese.table_script_priority(),
            Some(&[*b"java", *b"DFLT"][..])
        );
        assert_eq!(Script::Arabic.table_script_priority(), None);
    }

    #[test]
    fn lookup_is_case_insensitive() {
        assert_eq!(Script::from_iso15924_tag(*b"ARAB"), Some(Script::Arabic));
        assert_eq!(Script::from_iso15924_tag(*b"laoo"), Some(Script::Lao));
        assert_eq!(
            Direction::horizontal_for_script(*b"hebr"),
            Some(Direction::Rtl)
        );
    }

    #[test]
    fn aliases_fold_into_buckets() {
        assert_eq!(Script::from_iso15924_tag(*b"Hant"), Some(Script::Han));
        assert_eq!(Script::from_iso15924_tag(*b"Kore"), Some(Script::Hangul));
        assert_eq!(Script::from_iso15924_tag(*b"Syrc"), Some(Script::Syriac));
        assert_eq!(Script::from_iso15924_tag(*b"Armn"), None);
        assert_eq!(Script::from_iso15924_tag(*b"Zinh"), None);
    }

    #[test]
    fn rtl_scripts_match_harfbuzz() {
        for code in RTL_CODES {
            assert_eq!(
                Direction::horizontal_for_script(**code),
                Some(Direction::Rtl)
            );
        }
        for code in BIDIRECTIONAL_CODES {
            assert_eq!(Direction::horizontal_for_script(**code), None);
        }
        assert_eq!(
            Direction::horizontal_for_script(*b"Zzzz"),
            Some(Direction::Ltr)
        );
        assert_eq!(Script::NKo.horizontal_direction(), Direction::Rtl);
        assert_eq!(Script::Arabic.horizontal_direction(), Direction::Rtl);
        assert_eq!(Script::Mongolian.horizontal_direction(), Direction::Ltr);
        assert_eq!(Script::Other.horizontal_direction(), Direction::Ltr);
    }
}
