//! BCP 47 language tags and the OpenType language systems they select.
//!
//! OpenType fonts group GSUB and GPOS features by script, then by
//! language system (`LangSys`). A Serbian font can ship a `locl`
//! lookup under `cyrl`/`SRB ` that swaps in the Serbian forms of б and
//! г, and a Turkish font can keep the dotted `i` out of an `fi`
//! ligature under `latn`/`TRK `. [`Language`] carries the BCP 47 tag
//! a caller sets on a [`crate::Buffer`], and
//! [`Language::ot_language_tags`] lists the language system tags the
//! shaper tries for it, most specific first.
//!
//! # Where the mapping comes from
//!
//! The table in `table.rs` is generated from three committed snapshots
//! under `tests/tools/langtags/`:
//!
//! - The Microsoft OpenType Language System Tag registry, the primary
//!   source (<https://learn.microsoft.com/en-us/typography/opentype/spec/languagetags>).
//!   Each row lists a tag and the ISO 639 codes it covers.
//! - SIL's ISO 639-3 code table, reduced to the codes that have an ISO
//!   639-1 two-letter form (BCP 47 requires the two-letter form).
//! - SIL's ISO 639-3 macrolanguage table, so an individual language
//!   without its own row inherits its macrolanguage's tags (Egyptian
//!   Arabic `arz` gets Arabic `ARA `).
//!
//! All three were retrieved on 2026-09-25; the header of `table.rs`
//! repeats the URLs. Regenerate the table with
//!
//! ```text
//! cargo test --test language_table_gen -- --ignored
//! ```
//!
//! and see `tests/language_table_gen.rs` for how to refresh the
//! snapshots and for the few deliberate departures from the registry
//! (for example, a bare `zh` means Simplified Chinese). The generated
//! table was cross-checked against rustybuzz 0.20's HarfBuzz-derived
//! table: the first-choice tag agrees for all but a few dozen rare
//! subtags, mostly regional Quechua varieties that HarfBuzz groups
//! from sources outside the registry.
//!
//! # Tag structure
//!
//! Beyond the primary language subtag, the resolver understands the
//! parts of BCP 47 that HarfBuzz special-cases:
//!
//! - Chinese script and region subtags: `zh-Hant` and `zh-TW` select
//!   `ZHT `, `zh-HK` selects `ZHH `, `zh-MO` selects `ZHTM` then
//!   `ZHH `, and `zh-Hans`, `zh-CN`, `zh-SG`, or plain `zh` select
//!   `ZHS `. The same rules apply to every member of the Chinese
//!   macrolanguage (`cmn`, `yue`, `hak`, ...).
//! - Variant and script subtags the registry names: `-fonipa`
//!   (`IPPH`), `-fonnapa` (`APPH`), `-fonupa` (`UPPH`), `-Syre`,
//!   `-Syrj`, `-Syrn`, plus HarfBuzz's `-polyton` (`PGR `), `-Geok`
//!   (`KGE `), `-arevmda`, `-provenc`, `ga-Latg`, `ro-MD`, and
//!   `mnw-TH`.
//! - Extended language subtags (`zh-yue` resolves like `yue`) and a few
//!   irregular legacy tags (`i-lux`, `art-lojban`, `no-bok`).
//! - HarfBuzz's private-use override: `-x-hbot-TAG` (written without a
//!   hyphen after `hbot`, as in `en-x-hbotabc`) names the language
//!   system tag directly.
//! - An unknown three-letter subtag falls back to its uppercase form
//!   (`xyz` tries `XYZ `), unless that string is a registered tag for a
//!   different language.

use alloc::sync::Arc;
use core::cmp::Ordering;
use core::fmt;
use core::hash::{Hash, Hasher};

#[rustfmt::skip]
mod table;

use table::{CHINESE_FAMILY, LANGUAGE_TAGS, NO_UPPERCASE_FALLBACK, SUBTAG_TAGS};

/// A BCP 47 language tag, normalized for comparison.
///
/// Normalization follows HarfBuzz's `hb_language_from_string`: ASCII
/// letters are lowercased, `_` becomes `-`, and the tag ends at the
/// first byte that is not an ASCII letter, digit, `-`, or `_` (so
/// POSIX locale names like `tr_TR.UTF-8` reduce to `tr-tr`).
///
/// Cloning is cheap: the tag string is shared behind an [`Arc`] and
/// the OpenType tags are resolved once, at construction.
///
/// # Examples
///
/// ```
/// use sigilbuzz::Language;
///
/// let sr = Language::new("sr_Cyrl").expect("non-empty tag");
/// assert_eq!(sr.as_str(), "sr-cyrl");
/// assert_eq!(sr.ot_language_tags(), &[*b"SRB "]);
///
/// let hk = Language::new("zh-Hant-HK").expect("non-empty tag");
/// assert_eq!(hk.ot_language_tags(), &[*b"ZHH "]);
/// ```
#[derive(Clone)]
pub struct Language {
    tag: Arc<str>,
    ot_tags: OtTags,
}

/// The resolved OpenType language system tags for a [`Language`].
#[derive(Clone, Copy)]
enum OtTags {
    None,
    Static(&'static [[u8; 4]]),
    One([u8; 4]),
}

/// Most OpenType language system tags tried for one language, as in
/// HarfBuzz's `HB_OT_MAX_TAGS_PER_LANGUAGE`. The shaper asks for this
/// many candidates, so a tag past the third is never selected.
const MAX_TAGS_PER_LANGUAGE: usize = 3;

impl OtTags {
    fn as_slice(&self) -> &[[u8; 4]] {
        let tags = match self {
            Self::None => &[],
            Self::Static(tags) => *tags,
            Self::One(tag) => core::slice::from_ref(tag),
        };
        &tags[..tags.len().min(MAX_TAGS_PER_LANGUAGE)]
    }
}

impl Language {
    /// Parses and normalizes a BCP 47 language tag.
    ///
    /// Returns `None` when nothing is left after normalization (an
    /// empty string, or one that starts with a byte outside the tag
    /// alphabet), matching HarfBuzz's `HB_LANGUAGE_INVALID`.
    ///
    /// # Examples
    ///
    /// ```
    /// use sigilbuzz::Language;
    ///
    /// assert_eq!(Language::new("en_US").map(|l| l.as_str().to_owned()).as_deref(), Some("en-us"));
    /// assert!(Language::new("").is_none());
    /// ```
    #[must_use]
    pub fn new(tag: &str) -> Option<Self> {
        let normalized: alloc::string::String = tag
            .bytes()
            .map_while(|b| match b {
                b'-' | b'_' => Some('-'),
                b if b.is_ascii_alphanumeric() => Some(char::from(b.to_ascii_lowercase())),
                _ => None,
            })
            .collect();
        if normalized.is_empty() {
            return None;
        }
        let ot_tags = resolve(&normalized);
        Some(Self {
            tag: Arc::from(normalized),
            ot_tags,
        })
    }

    /// The normalized tag, for example `"zh-hant-hk"`.
    ///
    /// # Examples
    ///
    /// ```
    /// use sigilbuzz::Language;
    ///
    /// let lang = Language::new("pt-BR").expect("non-empty tag");
    /// assert_eq!(lang.as_str(), "pt-br");
    /// ```
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.tag
    }

    /// The OpenType language system tags this language selects, most
    /// specific first. The shaper tries each in order under the run's
    /// script and falls back to the script's default language system
    /// when the font has none of them. Empty when the tag maps to no
    /// registered language system.
    ///
    /// Like HarfBuzz (`HB_OT_MAX_TAGS_PER_LANGUAGE`), at most three
    /// candidates are returned; further tags the registry lists for
    /// the language are never tried.
    ///
    /// # Examples
    ///
    /// ```
    /// use sigilbuzz::Language;
    ///
    /// let hy = Language::new("hy").expect("non-empty tag");
    /// assert_eq!(hy.ot_language_tags(), &[*b"HYE0", *b"HYE "]);
    ///
    /// let custom = Language::new("en-x-hbotabcd").expect("non-empty tag");
    /// assert_eq!(custom.ot_language_tags(), &[*b"ABCD"]);
    ///
    /// let unknown = Language::new("xy").expect("non-empty tag");
    /// assert!(unknown.ot_language_tags().is_empty());
    /// ```
    #[must_use]
    pub fn ot_language_tags(&self) -> &[[u8; 4]] {
        self.ot_tags.as_slice()
    }
}

impl fmt::Debug for Language {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_tuple("Language").field(&self.as_str()).finish()
    }
}

impl fmt::Display for Language {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

// Equality, ordering, and hashing look at the normalized tag only; the
// OpenType tags are a pure function of it.
impl PartialEq for Language {
    fn eq(&self, other: &Self) -> bool {
        self.tag == other.tag
    }
}

impl Eq for Language {}

impl PartialOrd for Language {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for Language {
    fn cmp(&self, other: &Self) -> Ordering {
        self.tag.cmp(&other.tag)
    }
}

impl Hash for Language {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.tag.hash(state);
    }
}

/// Resolves a normalized tag to its OpenType language system tags,
/// following HarfBuzz's `hb_ot_tags_from_script_and_language`.
fn resolve(lang: &str) -> OtTags {
    let (prefix, private_use) = split_private_use(lang);
    if let Some(tag) = private_use.and_then(hbot_tag) {
        return OtTags::One(tag);
    }
    if prefix.is_empty() {
        return OtTags::None;
    }
    if let Some(tags) = complex_tags(prefix) {
        return OtTags::Static(tags);
    }
    let sub = first_subtag(language_subtag(prefix));
    if let Ok(i) = LANGUAGE_TAGS.binary_search_by(|(code, _)| (*code).cmp(sub)) {
        return OtTags::Static(LANGUAGE_TAGS[i].1);
    }
    let bytes = sub.as_bytes();
    if bytes.len() == 3
        && bytes.iter().all(u8::is_ascii_lowercase)
        && NO_UPPERCASE_FALLBACK.binary_search(&sub).is_err()
    {
        let upper = [
            bytes[0].to_ascii_uppercase(),
            bytes[1].to_ascii_uppercase(),
            bytes[2].to_ascii_uppercase(),
            b' ',
        ];
        return OtTags::One(upper);
    }
    OtTags::None
}

/// Splits `lang` into the part before the first singleton subtag
/// (extensions and private use are not language data) and the
/// private-use part starting at its `x`, if any.
fn split_private_use(lang: &str) -> (&str, Option<&str>) {
    if lang.starts_with("x-") {
        return ("", Some(lang));
    }
    let bytes = lang.as_bytes();
    let mut limit = None;
    for i in 1..bytes.len().saturating_sub(1) {
        if bytes[i - 1] == b'-' && bytes[i + 1] == b'-' {
            if bytes[i] == b'x' {
                return (&lang[..limit.unwrap_or(i - 1)], Some(&lang[i..]));
            }
            limit.get_or_insert(i - 1);
        }
    }
    (&lang[..limit.unwrap_or(lang.len())], None)
}

/// HarfBuzz's private-use override: `-hbot` followed directly by up to
/// four letters or digits names the language system tag.
fn hbot_tag(private_use: &str) -> Option<[u8; 4]> {
    let start = private_use.find("-hbot")? + "-hbot".len();
    let mut tag = [b' '; 4];
    let mut len = 0;
    for b in private_use[start..].bytes().take(4) {
        if !b.is_ascii_alphanumeric() {
            break;
        }
        tag[len] = b.to_ascii_uppercase();
        len += 1;
    }
    if len == 0 {
        return None;
    }
    // HarfBuzz reserves 'DFLT' for scripts; as a language it means the
    // lowercase 'dflt' record.
    if &tag == b"DFLT" {
        tag = *b"dflt";
    }
    Some(tag)
}

/// The subtag to look up: an extended language subtag when present
/// (`zh-yue` looks up `yue`), otherwise the primary subtag.
fn language_subtag(prefix: &str) -> &str {
    let Some(dash) = prefix.find('-') else {
        return prefix;
    };
    let rest = &prefix[dash + 1..];
    let next_len = rest.find('-').unwrap_or(rest.len());
    if prefix.len() >= 6 && next_len == 3 && rest.as_bytes()[0].is_ascii_alphabetic() {
        rest
    } else {
        prefix
    }
}

fn first_subtag(s: &str) -> &str {
    s.split('-').next().unwrap_or(s)
}

/// True when `lang` contains `-subtag` as a whole subtag.
fn has_subtag(lang: &str, subtag: &str) -> bool {
    lang.split('-').skip(1).any(|s| s == subtag)
}

/// True when `lang` starts with the subtag sequence `spec`, for
/// example `zh-hant-hk` matches `zh-hant-hk-x` but not `zh-hant-hkg`.
fn starts_with_subtags(lang: &str, spec: &str) -> bool {
    lang.strip_prefix(spec)
        .is_some_and(|rest| rest.is_empty() || rest.starts_with('-'))
}

const ZHS: &[[u8; 4]] = &[*b"ZHS "];
const ZHT: &[[u8; 4]] = &[*b"ZHT "];
const ZHH: &[[u8; 4]] = &[*b"ZHH "];
const ZHTM_ZHH: &[[u8; 4]] = &[*b"ZHTM", *b"ZHH "];

/// Variant and script subtags HarfBuzz maps beyond the ones the
/// registry names (those come from the generated `SUBTAG_TAGS`).
const EXTRA_SUBTAG_TAGS: &[(&str, &[[u8; 4]])] = &[
    ("arevmda", &[*b"HYE "]),
    ("geok", &[*b"KGE "]),
    ("polyton", &[*b"PGR "]),
    ("provenc", &[*b"PRO "]),
];

/// Irregular and legacy tags matched as a whole.
const IRREGULAR_TAGS: &[(&str, &[[u8; 4]])] = &[
    ("art-lojban", &[*b"JBO "]),
    ("i-hak", ZHS),
    ("i-lux", &[*b"LTZ "]),
    ("i-navajo", &[*b"NAV ", *b"ATH "]),
    ("no-bok", &[*b"NOR "]),
    ("no-nyn", &[*b"NYN "]),
    ("zh-min", ZHS),
    ("zh-min-nan", ZHS),
];

/// Tags whose meaning depends on more than the language subtag.
fn complex_tags(lang: &str) -> Option<&'static [[u8; 4]]> {
    for (subtag, tag) in SUBTAG_TAGS {
        if has_subtag(lang, subtag) {
            return Some(core::slice::from_ref(tag));
        }
    }
    for (subtag, tags) in EXTRA_SUBTAG_TAGS {
        if has_subtag(lang, subtag) {
            return Some(tags);
        }
    }
    if let Some((_, tags)) = IRREGULAR_TAGS.iter().find(|(t, _)| *t == lang) {
        return Some(tags);
    }
    let primary = first_subtag(lang);
    if CHINESE_FAMILY.binary_search(&primary).is_ok() {
        return chinese_tags(lang, primary);
    }
    const IRT: &[[u8; 4]] = &[*b"IRT "];
    const MONT: &[[u8; 4]] = &[*b"MONT"];
    const MOL_ROM: &[[u8; 4]] = &[*b"MOL ", *b"ROM "];
    match primary {
        "ga" if starts_with_subtags(lang, "ga-latg") => Some(IRT),
        "mnw" if has_subtag(lang, "th") => Some(MONT),
        "ro" if has_subtag(lang, "md") => Some(MOL_ROM),
        _ => None,
    }
}

/// Script and region handling for the Chinese macrolanguage family.
fn chinese_tags(lang: &str, primary: &str) -> Option<&'static [[u8; 4]]> {
    let rest = &lang[primary.len()..];
    let script_is = |spec: &str| starts_with_subtags(rest, spec);
    if script_is("-hans") {
        return Some(ZHS);
    }
    // Cantonese and Literary Chinese are traditional by default; only
    // an explicit Simplified script subtag moves them.
    if matches!(primary, "yue" | "lzh") {
        return None;
    }
    if script_is("-hant-hk") {
        return Some(ZHH);
    }
    if script_is("-hant-mo") {
        return Some(ZHTM_ZHH);
    }
    if script_is("-hant") {
        return Some(ZHT);
    }
    if has_subtag(lang, "hk") {
        return Some(ZHH);
    }
    if has_subtag(lang, "mo") {
        return Some(ZHTM_ZHH);
    }
    if has_subtag(lang, "tw") {
        return Some(ZHT);
    }
    None
}

#[cfg(test)]
mod tests;
