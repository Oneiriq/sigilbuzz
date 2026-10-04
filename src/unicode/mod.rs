//! Unicode property data that the shaper needs.
//!
//! Scripts (Latin, Arabic, Hangul, ...) and general categories drive
//! which feature list the shaper applies and how cluster boundaries
//! are decided. The bidi class, paired bracket, general category,
//! joining type, mirroring, Script, and canonical normalization tables
//! are generated from UCD snapshots (see `tests/unicode_table_gen.rs`).
//! The others are hand-curated excerpts of the UCD that cover the
//! scripts the shaper handles, not the full database.

pub mod bidi;
pub mod bidi_brackets;
#[rustfmt::skip]
mod bidi_brackets_table;
pub mod bidi_class;
#[rustfmt::skip]
mod bidi_class_table;
pub mod general_category;
#[rustfmt::skip]
mod general_category_table;
pub mod joining;
#[rustfmt::skip]
mod joining_table;
pub mod mirroring;
#[rustfmt::skip]
mod mirroring_table;
pub mod normalize;
#[rustfmt::skip]
mod script_table;
mod script_tags;

/// Coarse script classification.
///
/// One bucket for each script HarfBuzz gives a shaper of its own
/// (`hb_ot_shaper_categorize`), and a few more whose OpenType script
/// tags fonts use (Latin, Greek, Cyrillic, Han). Every other script is
/// [`Script::Other`]. A character's bucket is that of its Unicode Script
/// property (see [`script_of`]).
///
/// The enum is `#[non_exhaustive]`: later releases add buckets
/// without a breaking change, so a `match` outside this crate needs a
/// wildcard arm.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum Script {
    /// Latin (`Latn`), including Latin Extended Additional (Vietnamese)
    /// and the other Latin blocks.
    Latin,
    /// Han (`Hani`): the CJK ideographs of every extension, and
    /// Hiragana (`Hira`) and Katakana (`Kana`).
    Han,
    /// Arabic (`Arab`): Arabic, Persian, Urdu, the Arabic Supplement
    /// and Extended blocks, and the presentation forms.
    Arabic,
    /// Hebrew.
    Hebrew,
    /// Cyrillic.
    Cyrillic,
    /// Greek.
    Greek,
    /// Devanagari. Indic reordering shaper applies.
    Devanagari,
    /// Bengali. Indic reordering shaper applies.
    Bengali,
    /// Gurmukhi. Indic reordering shaper applies.
    Gurmukhi,
    /// Gujarati. Indic reordering shaper applies.
    Gujarati,
    /// Oriya. Indic reordering shaper applies.
    Oriya,
    /// Tamil. Indic reordering shaper applies.
    Tamil,
    /// Telugu. Indic reordering shaper applies.
    Telugu,
    /// Kannada. Indic reordering shaper applies.
    Kannada,
    /// Malayalam. Indic reordering shaper applies.
    Malayalam,
    /// Sinhala. The Universal Shaping Engine (USE) applies, as in
    /// HarfBuzz.
    Sinhala,
    /// Khmer. The Khmer shaper applies.
    Khmer,
    /// Myanmar (Burmese, Shan, Mon). The Myanmar shaper applies.
    Myanmar,
    /// Thai. The Thai shaper applies: the default features, after
    /// sara am splits into nikhahit and sara aa.
    Thai,
    /// Lao. Shaped like Thai.
    Lao,
    /// Hangul. A buffer of Hangul runs the Hangul shaper, which
    /// composes and decomposes syllables as the font needs and gives
    /// the jamo of a syllable left decomposed (U+1100..U+11FF,
    /// U+A960..U+A97F, U+D7B0..U+D7FF) the `ljmo` / `vjmo` / `tjmo`
    /// features.
    Hangul,
    /// Tibetan. Stacked above/below-base subjoined consonants. The
    /// Universal Shaping Engine applies, as in HarfBuzz.
    Tibetan,
    /// Mongolian, the Mongolian Supplement included. Cursive-joining
    /// like Arabic, with Free Variation Selectors (U+180B..U+180D,
    /// U+180F) overriding the joining-form choice. The Universal
    /// Shaping Engine applies, with the joining forms of
    /// [`crate::ot::mongolian`].
    Mongolian,
    /// N'Ko. Right-to-left alphabetic script for the Manding language
    /// family (Bambara / Maninka / Dyula). USE pipeline.
    NKo,
    /// Buginese (Lontara). Brahmic script for the Bugis language of
    /// South Sulawesi. USE pipeline.
    Buginese,
    /// Tai Tham (Lanna). Brahmic script used for Northern Thai, Tai
    /// Lue, Khün, and Lao Tham. USE pipeline.
    TaiTham,
    /// Balinese. Brahmic script for Balinese / Sasak / Old Javanese.
    /// USE pipeline.
    Balinese,
    /// Sundanese. Brahmic script for the Sundanese language of West
    /// Java. USE pipeline.
    Sundanese,
    /// Lepcha. Brahmic script of Sikkim used for the Lepcha language.
    /// USE pipeline.
    Lepcha,
    /// Limbu. Brahmic-derived script of Sikkim / Eastern Nepal used
    /// for the Limbu language. USE pipeline.
    Limbu,
    /// Cham. Brahmic script of Cambodia and Vietnam used for the
    /// Cham language. USE pipeline.
    Cham,
    /// Brahmi. The 3rd-century-BCE ancestor of every Brahmic script.
    /// Historical / scholarly use only. USE pipeline.
    Brahmi,
    /// Sharada. Historical Kashmiri / Sanskrit script (8th century).
    /// Still used liturgically. USE pipeline.
    Sharada,
    /// Khojki. Historical script for the Sindhi / Khoja Ismaili
    /// community. USE pipeline.
    Khojki,
    /// Tirhuta. Historical script for Maithili / Sanskrit. USE pipeline.
    Tirhuta,
    /// Modi. Historical script for Marathi (17th century). USE pipeline.
    Modi,
    /// Syriac (`Syrc`). The Arabic shaper applies, with the Syriac joining forms.
    Syriac,
    /// Buhid (`Buhd`). USE pipeline.
    Buhid,
    /// Hanunoo (`Hano`). USE pipeline.
    Hanunoo,
    /// Tagalog (`Tglg`). USE pipeline.
    Tagalog,
    /// Tagbanwa (`Tagb`). USE pipeline.
    Tagbanwa,
    /// Tai Le (`Tale`). USE pipeline.
    TaiLe,
    /// Kharoshthi (`Khar`). USE pipeline.
    Kharoshthi,
    /// Syloti Nagri (`Sylo`). USE pipeline.
    SylotiNagri,
    /// Tifinagh (`Tfng`). USE pipeline.
    Tifinagh,
    /// Phags Pa (`Phag`). USE pipeline, with Arabic-style joining forms.
    PhagsPa,
    /// Kayah Li (`Kali`). USE pipeline.
    KayahLi,
    /// Rejang (`Rjng`). USE pipeline.
    Rejang,
    /// Saurashtra (`Saur`). USE pipeline.
    Saurashtra,
    /// Egyptian Hieroglyphs (`Egyp`). USE pipeline.
    EgyptianHieroglyphs,
    /// Javanese (`Java`). USE pipeline.
    Javanese,
    /// Kaithi (`Kthi`). USE pipeline.
    Kaithi,
    /// Meetei Mayek (`Mtei`). USE pipeline.
    MeeteiMayek,
    /// Tai Viet (`Tavt`). USE pipeline.
    TaiViet,
    /// Batak (`Batk`). USE pipeline.
    Batak,
    /// Mandaic (`Mand`). USE pipeline, with Arabic-style joining forms.
    Mandaic,
    /// Chakma (`Cakm`). USE pipeline.
    Chakma,
    /// Miao (`Plrd`). USE pipeline.
    Miao,
    /// Takri (`Takr`). USE pipeline.
    Takri,
    /// Duployan (`Dupl`). USE pipeline.
    Duployan,
    /// Grantha (`Gran`). USE pipeline.
    Grantha,
    /// Khudawadi (`Sind`). USE pipeline.
    Khudawadi,
    /// Mahajani (`Mahj`). USE pipeline.
    Mahajani,
    /// Manichaean (`Mani`). USE pipeline, with Arabic-style joining forms.
    Manichaean,
    /// Pahawh Hmong (`Hmng`). USE pipeline.
    PahawhHmong,
    /// Psalter Pahlavi (`Phlp`). USE pipeline, with Arabic-style joining forms.
    PsalterPahlavi,
    /// Siddham (`Sidd`). USE pipeline.
    Siddham,
    /// Ahom (`Ahom`). USE pipeline.
    Ahom,
    /// Multani (`Mult`). USE pipeline.
    Multani,
    /// Adlam (`Adlm`). USE pipeline, with Arabic-style joining forms.
    Adlam,
    /// Bhaiksuki (`Bhks`). USE pipeline.
    Bhaiksuki,
    /// Marchen (`Marc`). USE pipeline.
    Marchen,
    /// Newa (`Newa`). USE pipeline.
    Newa,
    /// Masaram Gondi (`Gonm`). USE pipeline.
    MasaramGondi,
    /// Soyombo (`Soyo`). USE pipeline.
    Soyombo,
    /// Zanabazar Square (`Zanb`). USE pipeline.
    ZanabazarSquare,
    /// Dogra (`Dogr`). USE pipeline.
    Dogra,
    /// Gunjala Gondi (`Gong`). USE pipeline.
    GunjalaGondi,
    /// Hanifi Rohingya (`Rohg`). USE pipeline, with Arabic-style joining forms.
    HanifiRohingya,
    /// Makasar (`Maka`). USE pipeline.
    Makasar,
    /// Medefaidrin (`Medf`). USE pipeline.
    Medefaidrin,
    /// Old Sogdian (`Sogo`). USE pipeline.
    OldSogdian,
    /// Sogdian (`Sogd`). USE pipeline, with Arabic-style joining forms.
    Sogdian,
    /// Elymaic (`Elym`). USE pipeline.
    Elymaic,
    /// Nandinagari (`Nand`). USE pipeline.
    Nandinagari,
    /// Nyiakeng Puachue Hmong (`Hmnp`). USE pipeline.
    NyiakengPuachueHmong,
    /// Wancho (`Wcho`). USE pipeline.
    Wancho,
    /// Chorasmian (`Chrs`). USE pipeline, with Arabic-style joining forms.
    Chorasmian,
    /// Dives Akuru (`Diak`). USE pipeline.
    DivesAkuru,
    /// Khitan Small Script (`Kits`). USE pipeline.
    KhitanSmallScript,
    /// Yezidi (`Yezi`). USE pipeline.
    Yezidi,
    /// Cypro Minoan (`Cpmn`). USE pipeline.
    CyproMinoan,
    /// Old Uyghur (`Ougr`). USE pipeline, with Arabic-style joining forms.
    OldUyghur,
    /// Tangsa (`Tnsa`). USE pipeline.
    Tangsa,
    /// Toto (`Toto`). USE pipeline.
    Toto,
    /// Vithkuqi (`Vith`). USE pipeline.
    Vithkuqi,
    /// Kawi (`Kawi`). USE pipeline.
    Kawi,
    /// Nag Mundari (`Nagm`). USE pipeline.
    NagMundari,
    /// Garay (`Gara`). USE pipeline.
    Garay,
    /// Gurung Khema (`Gukh`). USE pipeline.
    GurungKhema,
    /// Kirat Rai (`Krai`). USE pipeline.
    KiratRai,
    /// Ol Onal (`Onao`). USE pipeline.
    OlOnal,
    /// Sunuwar (`Sunu`). USE pipeline.
    Sunuwar,
    /// Todhri (`Todr`). USE pipeline.
    Todhri,
    /// Tulu Tigalari (`Tutg`). USE pipeline.
    TuluTigalari,
    /// Beria Erfe (`Berf`). USE pipeline.
    BeriaErfe,
    /// Sidetic (`Sidt`). USE pipeline.
    Sidetic,
    /// Tai Yo (`Tayo`). USE pipeline.
    TaiYo,
    /// Tolong Siki (`Tols`). USE pipeline.
    TolongSiki,
    /// Jurchen (`Jurc`). USE pipeline.
    Jurchen,
    /// Proto Cuneiform (`Pcun`). USE pipeline.
    ProtoCuneiform,
    /// Seal (`Seal`). USE pipeline.
    Seal,
    /// Anything else: returned when sigilbuzz has no specialized
    /// table for the codepoint's script.
    Other,
}

impl Script {
    /// Returns `true` if the script is one of the Indic family scripts
    /// that run through the Indic reordering shaper: Devanagari,
    /// Bengali, Gurmukhi, Gujarati, Oriya, Tamil, Telugu, Kannada, and
    /// Malayalam, as in HarfBuzz (`hb_ot_shaper_categorize`). Sinhala
    /// runs the Universal Shaping Engine (see [`Self::is_use`]).
    #[must_use]
    pub const fn is_indic(self) -> bool {
        matches!(
            self,
            Script::Devanagari
                | Script::Bengali
                | Script::Gurmukhi
                | Script::Gujarati
                | Script::Oriya
                | Script::Tamil
                | Script::Telugu
                | Script::Kannada
                | Script::Malayalam
        )
    }

    /// Returns `true` if the script routes through the Universal
    /// Shaping Engine, as in HarfBuzz (`hb_ot_shaper_categorize`):
    /// Sinhala, Tibetan, Mongolian, N'Ko, the Brahmic SE-Asian / South
    /// Asian set (Buginese, Tai Tham, Balinese, Javanese, ...), the
    /// Brahmi-family historical scripts (Brahmi, Kaithi, Takri, ...),
    /// and the joining scripts other than Arabic and Syriac (Adlam,
    /// Mandaic, Sogdian, ...). A font whose GSUB has lookups for such a
    /// script only under `DFLT` or `latn` gets the default shaper
    /// instead. Khmer, Myanmar, Thai, Lao, and Hangul have shapers of
    /// their own, and Syriac takes the Arabic shaper.
    ///
    /// # Examples
    ///
    /// ```
    /// use sigilbuzz::UnicodeScript;
    ///
    /// assert!(UnicodeScript::Javanese.is_use());
    /// assert!(!UnicodeScript::Syriac.is_use());
    /// ```
    #[must_use]
    pub const fn is_use(self) -> bool {
        matches!(
            self,
            Script::Sinhala
                | Script::Tibetan
                | Script::Mongolian
                | Script::NKo
                | Script::Buginese
                | Script::TaiTham
                | Script::Balinese
                | Script::Sundanese
                | Script::Lepcha
                | Script::Limbu
                | Script::Cham
                | Script::Brahmi
                | Script::Sharada
                | Script::Khojki
                | Script::Tirhuta
                | Script::Modi
                | Script::Buhid
                | Script::Hanunoo
                | Script::Tagalog
                | Script::Tagbanwa
                | Script::TaiLe
                | Script::Kharoshthi
                | Script::SylotiNagri
                | Script::Tifinagh
                | Script::PhagsPa
                | Script::KayahLi
                | Script::Rejang
                | Script::Saurashtra
                | Script::EgyptianHieroglyphs
                | Script::Javanese
                | Script::Kaithi
                | Script::MeeteiMayek
                | Script::TaiViet
                | Script::Batak
                | Script::Mandaic
                | Script::Chakma
                | Script::Miao
                | Script::Takri
                | Script::Duployan
                | Script::Grantha
                | Script::Khudawadi
                | Script::Mahajani
                | Script::Manichaean
                | Script::PahawhHmong
                | Script::PsalterPahlavi
                | Script::Siddham
                | Script::Ahom
                | Script::Multani
                | Script::Adlam
                | Script::Bhaiksuki
                | Script::Marchen
                | Script::Newa
                | Script::MasaramGondi
                | Script::Soyombo
                | Script::ZanabazarSquare
                | Script::Dogra
                | Script::GunjalaGondi
                | Script::HanifiRohingya
                | Script::Makasar
                | Script::Medefaidrin
                | Script::OldSogdian
                | Script::Sogdian
                | Script::Elymaic
                | Script::Nandinagari
                | Script::NyiakengPuachueHmong
                | Script::Wancho
                | Script::Chorasmian
                | Script::DivesAkuru
                | Script::KhitanSmallScript
                | Script::Yezidi
                | Script::CyproMinoan
                | Script::OldUyghur
                | Script::Tangsa
                | Script::Toto
                | Script::Vithkuqi
                | Script::Kawi
                | Script::NagMundari
                | Script::Garay
                | Script::GurungKhema
                | Script::KiratRai
                | Script::OlOnal
                | Script::Sunuwar
                | Script::Todhri
                | Script::TuluTigalari
                | Script::BeriaErfe
                | Script::Sidetic
                | Script::TaiYo
                | Script::TolongSiki
                | Script::Jurchen
                | Script::ProtoCuneiform
                | Script::Seal
        )
    }

    /// True for the scripts whose letters join like Arabic, which
    /// HarfBuzz's Arabic and Universal Shaping Engine shapers give
    /// joining forms (`has_arabic_joining` in
    /// `hb-ot-shaper-arabic-joining-list.hh`).
    pub(crate) const fn has_arabic_joining(self) -> bool {
        matches!(
            self,
            Script::Adlam
                | Script::Arabic
                | Script::Chorasmian
                | Script::HanifiRohingya
                | Script::Mandaic
                | Script::Manichaean
                | Script::Mongolian
                | Script::NKo
                | Script::OldUyghur
                | Script::PhagsPa
                | Script::PsalterPahlavi
                | Script::Sogdian
                | Script::Syriac
        )
    }
}

/// Returns the script bucket for a character: the bucket of its Unicode
/// Script property (`Scripts.txt` of Unicode 18.0.0, the version
/// HarfBuzz 14.5.0 uses), as HarfBuzz's `hb_unicode_script` reads it.
/// Hiragana and Katakana fall in [`Script::Han`]. Common (digits,
/// punctuation, the tatweel, the dandas), Inherited (combining marks),
/// unassigned and private-use code points, and the scripts sigilbuzz
/// has no bucket for are [`Script::Other`]: in a text, Common and
/// Inherited characters take the script of the text around them (see
/// [`crate::Buffer::script_runs`]).
///
/// Before 0.24.0 the older buckets (Latin through Modi, in declaration
/// order) covered the Unicode blocks of their scripts instead, so a
/// letter outside those blocks, such as Vietnamese U+1EF7 in Latin
/// Extended Additional or U+08A0 in Arabic Extended-A, was
/// [`Script::Other`], and the Common characters inside them took the
/// block's script.
///
/// # Examples
///
/// ```
/// use sigilbuzz::{script_of, UnicodeScript};
///
/// assert_eq!(script_of('\u{1EF7}'), UnicodeScript::Latin);
/// assert_eq!(script_of('\u{08A0}'), UnicodeScript::Arabic);
/// assert_eq!(script_of('\u{A98F}'), UnicodeScript::Javanese);
/// assert_eq!(script_of('\u{0712}'), UnicodeScript::Syriac);
/// assert_eq!(script_of('\u{0531}'), UnicodeScript::Other);
/// assert_eq!(script_of('1'), UnicodeScript::Other);
/// ```
#[must_use]
pub const fn script_of(ch: char) -> Script {
    let index = script_index(ch as u32) as usize;
    if index < script_table::BUCKETS.len() {
        script_table::BUCKETS[index]
    } else {
        Script::Other
    }
}

/// The index in `script_table::SCRIPT_TAGS` of the Unicode Script
/// property of code point `cp`: a binary search of the generated
/// ranges, `UNKNOWN` for a code point they do not list.
const fn script_index(cp: u32) -> u8 {
    let ranges = script_table::SCRIPT_RANGES;
    let (mut lo, mut hi) = (0, ranges.len());
    while lo < hi {
        let mid = lo + (hi - lo) / 2;
        let (start, end, script) = ranges[mid];
        if end < cp {
            lo = mid + 1;
        } else if start > cp {
            hi = mid;
        } else {
            return script;
        }
    }
    script_table::UNKNOWN
}

/// The Unicode Script property of `ch` as an ISO 15924 code: `Zyyy`
/// for Common, `Zinh` for Inherited, and `Zzzz` for unassigned code
/// points. This is what HarfBuzz's `hb_unicode_script` returns, from
/// the same Unicode version (18.0.0). [`script_of`] gives the coarser
/// shaping bucket.
///
/// # Examples
///
/// ```
/// use sigilbuzz::unicode::script_code;
///
/// assert_eq!(script_code('a'), *b"Latn");
/// assert_eq!(script_code('\u{0640}'), *b"Zyyy");
/// assert_eq!(script_code('\u{0301}'), *b"Zinh");
/// assert_eq!(script_code('\u{0378}'), *b"Zzzz");
/// ```
#[must_use]
pub fn script_code(ch: char) -> [u8; 4] {
    script_table::SCRIPT_TAGS
        .get(usize::from(script_index(u32::from(ch))))
        .copied()
        .unwrap_or(*b"Zzzz")
}

/// True when the Unicode Script property of `ch` is Common, Inherited,
/// or Unknown (unassigned and private-use code points), the scripts
/// HarfBuzz's `hb_buffer_guess_segment_properties` skips, so the
/// character takes the script of the text around it.
pub(crate) const fn is_scriptless(ch: char) -> bool {
    let index = script_index(ch as u32);
    index == script_table::COMMON
        || index == script_table::INHERITED
        || index == script_table::UNKNOWN
}

/// Returns `true` if the codepoint is a Hangul Jamo (Leading / Vowel /
/// Trailing / Extended-A / Extended-B), the subset of Hangul that USE
/// reorders via the `ljmo`/`vjmo`/`tjmo` features. Precomposed syllables
/// (U+AC00..U+D7A3) and Compatibility Jamo (U+3130..U+318F) stay on the
/// default path.
#[must_use]
pub const fn is_hangul_jamo(ch: char) -> bool {
    let cp = ch as u32;
    matches!(cp, 0x1100..=0x11FF | 0xA960..=0xA97F | 0xD7B0..=0xD7FF)
}

/// A Hangul tone mark, U+302E or U+302F (HarfBuzz's `isHangulTone`).
/// Its script is Hangul, and it is a combining mark (General_Category
/// Mc, combining class 224).
pub(crate) const fn is_hangul_tone_mark(ch: char) -> bool {
    matches!(ch as u32, 0x302E..=0x302F)
}

/// HarfBuzz's `hb_unicode_funcs_t::is_default_ignorable` (in
/// `hb-unicode.hh`): Default_Ignorable_Code_Point, except the Hangul
/// fillers (U+115F, U+1160, U+3164, U+FFA0) and the shorthand format
/// controls (U+1BCA0..U+1BCA3), which fonts draw as regular spacing
/// glyphs.
pub(crate) const fn is_default_ignorable(ch: char) -> bool {
    matches!(
        ch as u32,
        0x00AD // SOFT HYPHEN
            | 0x034F // COMBINING GRAPHEME JOINER
            | 0x061C // ARABIC LETTER MARK
            | 0x17B4..=0x17B5 // KHMER VOWEL INHERENT AQ, AA
            | 0x180B..=0x180F // MONGOLIAN FVS1..3, VOWEL SEPARATOR, FVS4
            | 0x200B..=0x200F // ZWSP, ZWNJ, ZWJ, LRM, RLM
            | 0x202A..=0x202E // bidi embeddings and overrides
            | 0x2060..=0x206F // word joiner, invisible operators, isolates
            | 0xFE00..=0xFE0F // variation selectors
            | 0xFEFF // ZERO WIDTH NO-BREAK SPACE
            | 0xFFF0..=0xFFF8 // reserved
            | 0x1D173..=0x1D17A // musical beam and phrase controls
            | 0xE0000..=0xE0FFF // tags and supplementary variation selectors
    )
}

/// True for a default ignorable (see [`is_default_ignorable`]) whose
/// script is Common or Inherited, so it extends the script run it sits
/// in: GSUB and GPOS match across it, which they cannot do when it
/// splits the run. The Khmer inherent vowels and the Mongolian
/// variation selectors have their own script and are left out.
pub(crate) const fn is_scriptless_default_ignorable(ch: char) -> bool {
    is_default_ignorable(ch) && !matches!(ch as u32, 0x17B4..=0x17B5 | 0x180B..=0x180D | 0x180F)
}

/// True for the characters that take the script of the text around
/// them when text splits into script runs, as HarfBuzz's buffer takes
/// the script of its first character that is not `COMMON`,
/// `INHERITED`, or `UNKNOWN`: every character the Unicode Script
/// property gives Common or Inherited (the tatweel, the dandas,
/// combining marks, digits, punctuation), the unassigned and
/// private-use code points (Unknown), and the default ignorables of no
/// script of their own, which GSUB and GPOS match across. Without
/// these, `"e\u{0301}"` would split into two runs and break `ccmp` and
/// any GSUB context across the mark.
///
/// The combining mark blocks sigilbuzz listed by hand before it read
/// the Script property stay in, so the two Cyrillic combining half
/// marks U+FE2E and U+FE2F never split from their base either.
pub(crate) const fn is_common_or_inherited(ch: char) -> bool {
    matches!(
        ch as u32,
        0x0300..=0x036F | 0x1DC0..=0x1DFF | 0x20D0..=0x20FF | 0xFE20..=0xFE2F
    ) || is_scriptless(ch)
        || is_scriptless_default_ignorable(ch)
}

#[cfg(test)]
mod tests;
