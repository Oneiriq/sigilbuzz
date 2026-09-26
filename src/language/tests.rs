//! Unit tests for BCP 47 resolution. Expected values for the
//! HarfBuzz-specific cases mirror rustybuzz 0.20's `tag.rs` tests,
//! which port HarfBuzz's own `test-ot-tag.c`.

use super::*;
use alloc::string::ToString;
use alloc::vec::Vec;

fn tags(lang: &str) -> Vec<[u8; 4]> {
    Language::new(lang).map_or_else(Vec::new, |l| l.ot_language_tags().to_vec())
}

fn first(lang: &str) -> Option<[u8; 4]> {
    tags(lang).first().copied()
}

#[test]
fn normalization_lowercases_and_maps_underscore() {
    let lang = Language::new("EN_us").unwrap();
    assert_eq!(lang.as_str(), "en-us");
    assert_eq!(lang.to_string(), "en-us");
    assert_eq!(Language::new("zH-HanT-hK").unwrap().as_str(), "zh-hant-hk");
}

#[test]
fn normalization_stops_at_first_foreign_byte() {
    // POSIX locale names carry a codeset and modifiers.
    assert_eq!(Language::new("tr_TR.UTF-8").unwrap().as_str(), "tr-tr");
    assert_eq!(Language::new("tr@foo=bar").unwrap().as_str(), "tr");
    assert_eq!(first("tr@foo=bar"), Some(*b"TRK "));
}

#[test]
fn empty_or_unparseable_tags_are_none() {
    assert!(Language::new("").is_none());
    assert!(Language::new("@").is_none());
    assert!(Language::new(".utf8").is_none());
}

#[test]
fn equality_and_hash_follow_the_normalized_tag() {
    use core::hash::BuildHasher;
    // A fixed-seed hasher keeps the test deterministic.
    struct Fnv;
    struct FnvHasher(u64);
    impl Hasher for FnvHasher {
        fn finish(&self) -> u64 {
            self.0
        }
        fn write(&mut self, bytes: &[u8]) {
            for b in bytes {
                self.0 = (self.0 ^ u64::from(*b)).wrapping_mul(0x0100_0000_01b3);
            }
        }
    }
    impl BuildHasher for Fnv {
        type Hasher = FnvHasher;
        fn build_hasher(&self) -> FnvHasher {
            FnvHasher(0xcbf2_9ce4_8422_2325)
        }
    }
    let a = Language::new("sr_Latn").unwrap();
    let b = Language::new("SR-latn").unwrap();
    let c = a.clone();
    assert_eq!(a, b);
    assert_eq!(a, c);
    assert_eq!(Fnv.hash_one(&a), Fnv.hash_one(&b));
    assert!(Language::new("ar").unwrap() < Language::new("en").unwrap());
    assert_eq!(alloc::format!("{a:?}"), "Language(\"sr-latn\")");
}

#[test]
fn two_and_three_letter_codes() {
    assert_eq!(tags("en"), [*b"ENG "]);
    assert_eq!(tags("eng"), [*b"ENG "]);
    assert_eq!(tags("tr"), [*b"TRK "]);
    assert_eq!(tags("tur"), [*b"TRK "]);
    assert_eq!(tags("sr"), [*b"SRB "]);
    assert_eq!(tags("mk"), [*b"MKD "]);
    assert_eq!(tags("ar"), [*b"ARA "]);
    assert_eq!(tags("fa"), [*b"FAR "]);
    assert_eq!(tags("ur"), [*b"URD "]);
    assert_eq!(tags("de-ch"), [*b"DEU "]);
    assert_eq!(tags("en-us"), [*b"ENG "]);
    assert_eq!(tags("fa-ir"), [*b"FAR "]);
}

#[test]
fn macrolanguage_members_inherit_tags() {
    // Egyptian and Standard Arabic have no registry row of their own.
    assert_eq!(tags("arz"), [*b"ARA "]);
    assert_eq!(tags("arb"), [*b"ARA "]);
    // Cypriot Arabic has its own tag and still falls back to Arabic.
    assert_eq!(tags("acy"), [*b"ACY ", *b"ARA "]);
    // Moroccan Arabic prefers the Moroccan tag.
    assert_eq!(tags("ary"), [*b"MOR ", *b"ARA "]);
    // Chinese members default to Simplified.
    assert_eq!(tags("cmn"), [*b"ZHS "]);
}

#[test]
fn multiple_tags_are_most_specific_first() {
    assert_eq!(tags("hy"), [*b"HYE0", *b"HYE "]);
    assert_eq!(tags("hyw"), [*b"HYE "]);
    assert_eq!(tags("ml"), [*b"MAL ", *b"MLR "]);
    assert_eq!(tags("dv"), [*b"DIV ", *b"DHV "]);
    assert_eq!(tags("aii"), [*b"SWA ", *b"SYR "]);
    assert_eq!(tags("id"), [*b"IND ", *b"MLY "]);
}

#[test]
fn chinese_script_and_region_subtags() {
    assert_eq!(tags("zh"), [*b"ZHS "]);
    assert_eq!(tags("zh-cn"), [*b"ZHS "]);
    assert_eq!(tags("zh-sg"), [*b"ZHS "]);
    assert_eq!(tags("zh-hans"), [*b"ZHS "]);
    assert_eq!(tags("zh-Hans-TW"), [*b"ZHS "]);
    assert_eq!(tags("zh-hans-mo"), [*b"ZHS "]);
    assert_eq!(tags("zH-HanS-hK"), [*b"ZHS "]);
    assert_eq!(tags("zh-Hant"), [*b"ZHT "]);
    assert_eq!(tags("zh-tw"), [*b"ZHT "]);
    assert_eq!(tags("zh-hant-tw"), [*b"ZHT "]);
    assert_eq!(tags("zh-HK"), [*b"ZHH "]);
    assert_eq!(tags("zH-HanT-hK"), [*b"ZHH "]);
    assert_eq!(tags("zh-mo"), [*b"ZHTM", *b"ZHH "]);
    assert_eq!(tags("zh-hant-mo"), [*b"ZHTM", *b"ZHH "]);
    assert_eq!(tags("zh-xx"), [*b"ZHS "]);
    assert_eq!(tags("cmn-hant"), [*b"ZHT "]);
    assert_eq!(tags("hak-tw"), [*b"ZHT "]);
}

#[test]
fn cantonese_and_literary_chinese_default_traditional() {
    assert_eq!(tags("yue"), [*b"ZHH "]);
    assert_eq!(tags("yue-Hant"), [*b"ZHH "]);
    assert_eq!(tags("yue-Hans"), [*b"ZHS "]);
    assert_eq!(tags("lzh"), [*b"ZHT "]);
    assert_eq!(tags("lzh-hans"), [*b"ZHS "]);
}

#[test]
fn extended_language_subtags() {
    assert_eq!(tags("zh-yue"), [*b"ZHH "]);
    assert_eq!(tags("ar-aao"), [*b"ARA "]);
    assert_eq!(tags("kok-gom"), [*b"KOK "]);
    assert_eq!(tags("ar-ary"), [*b"MOR ", *b"ARA "]);
    assert_eq!(tags("ar-ary-DZ"), [*b"MOR ", *b"ARA "]);
    // A UN M.49 region code is not an extended language subtag.
    assert_eq!(tags("ar-001"), [*b"ARA "]);
}

#[test]
fn private_use_hbot_overrides_the_language() {
    assert_eq!(tags("x-hbotabcd"), [*b"ABCD"]);
    assert_eq!(tags("en-x-hbotabc"), [*b"ABC "]);
    assert_eq!(tags("fa-x-hbotabc-zxc"), [*b"ABC "]);
    assert_eq!(tags("zh-cn-x-hbotabc-zxc"), [*b"ABC "]);
    assert_eq!(tags("asdf-asdf-wer-x-hbotabcd"), [*b"ABCD"]);
    assert_eq!(tags("x-hbot1234-hbsc5678"), [*b"1234"]);
    assert_eq!(tags("x-hbsc5678-hbot1234"), [*b"1234"]);
    assert_eq!(tags("x-hbotdflt"), [*b"dflt"]);
    // `-hbot` must be followed directly by the tag.
    assert!(tags("asdf-asdf-wer-x-hbot-zxc").is_empty());
    assert!(tags("x-hbot").is_empty());
}

#[test]
fn extensions_and_private_use_do_not_leak_into_matching() {
    assert_eq!(tags("en-x-fonipa"), [*b"ENG "]);
    assert_eq!(tags("en-a-fonipa"), [*b"ENG "]);
    assert_eq!(tags("en-a-qwe-b-fonipa"), [*b"ENG "]);
    assert_eq!(tags("en-fonipax"), [*b"ENG "]);
}

#[test]
fn variant_and_script_subtags() {
    assert_eq!(tags("en-fonipa"), [*b"IPPH"]);
    assert_eq!(tags("und-fonipa"), [*b"IPPH"]);
    assert_eq!(tags("rm-CH-fonipa-sursilv-x-foobar"), [*b"IPPH"]);
    assert_eq!(tags("chr-fonnapa"), [*b"APPH"]);
    assert_eq!(tags("fi-fonupa"), [*b"UPPH"]);
    assert_eq!(tags("el-polyton"), [*b"PGR "]);
    assert_eq!(tags("el-CY-polyton"), [*b"PGR "]);
    assert_eq!(tags("ka-Geok"), [*b"KGE "]);
    assert_eq!(tags("und-Geok"), [*b"KGE "]);
    assert_eq!(tags("ga-Latg"), [*b"IRT "]);
    assert_eq!(tags("syr-Syre"), [*b"SYRE"]);
    assert_eq!(tags("de-Syrj"), [*b"SYRJ"]);
    assert_eq!(tags("und-Syrn"), [*b"SYRN"]);
    assert_eq!(tags("ro-MD"), [*b"MOL ", *b"ROM "]);
    assert_eq!(tags("mnw-TH"), [*b"MONT"]);
    // Without the qualifying subtag the plain language tags apply.
    assert_eq!(tags("el"), [*b"ELL "]);
    assert_eq!(tags("ka"), [*b"KAT "]);
    assert_eq!(tags("syr"), [*b"SYR "]);
    assert_eq!(tags("ro"), [*b"ROM "]);
    assert_eq!(tags("mnw"), [*b"MON ", *b"MONT"]);
}

#[test]
fn irregular_and_deprecated_tags() {
    assert_eq!(tags("art-lojban"), [*b"JBO "]);
    assert_eq!(tags("i-lux"), [*b"LTZ "]);
    assert_eq!(tags("i-hak"), [*b"ZHS "]);
    assert_eq!(tags("i-navajo"), [*b"NAV ", *b"ATH "]);
    assert_eq!(tags("no-bok"), [*b"NOR "]);
    assert_eq!(tags("no-nyn"), [*b"NYN "]);
    assert_eq!(tags("zh-min-nan"), [*b"ZHS "]);
    assert_eq!(tags("no"), [*b"NOR "]);
    assert_eq!(tags("nb"), [*b"NOR "]);
    assert_eq!(tags("nn"), [*b"NYN "]);
    assert_eq!(tags("iw"), [*b"IWR "]);
    assert_eq!(tags("in"), [*b"IND ", *b"MLY "]);
    assert_eq!(tags("ji"), tags("yi"));
    assert_eq!(tags("mo"), [*b"MOL ", *b"ROM "]);
    assert_eq!(tags("sh"), [*b"BOS ", *b"HRV ", *b"SRB "]);
    assert_eq!(tags("bs"), [*b"BOS "]);
}

#[test]
fn unknown_codes() {
    // Two-letter codes outside ISO 639-1 select nothing.
    assert!(tags("xy").is_empty());
    assert!(tags("qq-latn").is_empty());
    // Longer primary subtags select nothing.
    assert!(tags("asdf").is_empty());
    // Unknown three-letter codes try their uppercase form.
    assert_eq!(tags("xyz"), [*b"XYZ "]);
    assert_eq!(tags("xyz-qw"), [*b"XYZ "]);
    // ...unless that is another language's registered tag: `aba` is
    // Abe, while 'ABA ' is Abaza; `far` is Fataleka, 'FAR ' is Persian.
    assert!(tags("aba").is_empty());
    assert!(tags("far").is_empty());
    // Tosk Albanian is a member of the Albanian macrolanguage.
    assert_eq!(tags("als"), [*b"SQI "]);
}

#[test]
fn generated_tables_are_sorted_and_well_formed() {
    let codes: Vec<&str> = LANGUAGE_TAGS.iter().map(|(c, _)| *c).collect();
    assert!(
        codes.windows(2).all(|w| w[0] < w[1]),
        "LANGUAGE_TAGS unsorted"
    );
    for (code, tags) in LANGUAGE_TAGS {
        assert!(!tags.is_empty(), "{code} has no tags");
        assert!((2..=3).contains(&code.len()), "odd code {code}");
        for tag in *tags {
            assert!(
                tag.iter().all(|b| b.is_ascii_graphic() || *b == b' '),
                "{tag:?}"
            );
        }
    }
    assert!(NO_UPPERCASE_FALLBACK.windows(2).all(|w| w[0] < w[1]));
    assert!(CHINESE_FAMILY.windows(2).all(|w| w[0] < w[1]));
    assert!(SUBTAG_TAGS.windows(2).all(|w| w[0].0 < w[1].0));
    for code in NO_UPPERCASE_FALLBACK {
        assert!(LANGUAGE_TAGS
            .binary_search_by(|(c, _)| c.cmp(code))
            .is_err());
    }
}

#[test]
fn split_private_use_matches_harfbuzz() {
    assert_eq!(split_private_use("x-hbotabc"), ("", Some("x-hbotabc")));
    assert_eq!(split_private_use("en-x-abc"), ("en", Some("x-abc")));
    assert_eq!(split_private_use("en-a-bcd-x-y"), ("en", Some("x-y")));
    assert_eq!(split_private_use("en-us"), ("en-us", None));
    assert_eq!(split_private_use("en-a-bcd"), ("en", None));
}
